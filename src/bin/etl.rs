//! Dev ETL — processes ALL exported Mongo data into contract Parquet.
//!
//! Reads the full Mongo export (every row, every served collection) from
//! /tmp/etl/*.jsonl, runs the per-dataset `DuckDB` transforms in
//! `/tmp/etl/etl_statements.json` (canonical ticker/date, daily date partitions,
//! flatten/explode/combine/filter per the field-map contract), and writes
//! Parquet under ./data. Each statement runs independently so one failure
//! doesn't abort the rest.
//!
//! Run all (clears ./data):      `cargo run --bin etl`
//! Re-run a subset (keeps rest): `cargo run --bin etl -- ownership summary`

use std::fs;

use duckdb::Connection;
use serde::Deserialize;

#[derive(Deserialize)]
struct Stmt {
    dataset: String,
    sql: String,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let all: Vec<Stmt> = serde_json::from_reader(fs::File::open("/tmp/etl/etl_statements.json")?)?;
    let stmts: Vec<&Stmt> = if args.is_empty() {
        all.iter().collect()
    } else {
        all.iter().filter(|s| args.contains(&s.dataset)).collect()
    };

    // A full run clears ./data; a subset run (dataset names as args) keeps it.
    if args.is_empty() {
        let _ = fs::remove_dir_all("data");
    }
    fs::create_dir_all("data")?;

    let conn = Connection::open_in_memory()?;
    // Spill into a temp dir, not the cwd `.tmp`. `preserve_insertion_order=false`
    // lets partitioned COPY stream instead of buffering the whole result, so a big
    // dataset (e.g. exploded broker_activity) doesn't blow up memory or thrash.
    let tmp = std::env::temp_dir().join("idx-etl-duck");
    fs::create_dir_all(&tmp).ok();
    conn.execute_batch(&format!(
        "SET threads TO 4; SET preserve_insertion_order=false; SET temp_directory='{}'; SET partitioned_write_max_open_files TO 2000;",
        tmp.display().to_string().replace('\'', "''")
    ))?;

    let mut ok = 0usize;
    for s in &stmts {
        // COPY TO a file does not create parent dirs; partitioned writes do.
        let _ = fs::create_dir_all(format!("data/{}", s.dataset));
        match conn.execute_batch(&s.sql) {
            Ok(()) => {
                let glob = if s.sql.contains("PARTITION_BY") {
                    format!("data/{}/date=*/*.parquet", s.dataset)
                } else {
                    format!("data/{}/latest.parquet", s.dataset)
                };
                let n: i64 = conn
                    .query_row(
                        &format!("SELECT count(*) FROM read_parquet('{glob}')"),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(-1);
                println!("OK    {:22} rows={n}", s.dataset);
                ok += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                let first = msg.lines().next().unwrap_or(&msg);
                println!("FAIL  {:22} {first}", s.dataset);
            }
        }
    }
    println!("--- {ok}/{} statements succeeded ---", stmts.len());
    Ok(())
}
