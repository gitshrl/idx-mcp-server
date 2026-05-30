//! Tiny ad-hoc `DuckDB` query tool for inspecting ./data. `cargo run --bin q -- "<sql>"`.
use duckdb::Connection;

fn main() -> anyhow::Result<()> {
    let sql = std::env::args().nth(1).expect("usage: q <sql>");
    let c = Connection::open_in_memory()?;
    let wrapped = format!("SELECT to_json(t)::VARCHAR FROM ({sql}) t");
    let mut s = c.prepare(&wrapped)?;
    let rows = s.query_map([], |r| r.get::<_, String>(0))?;
    for row in rows {
        println!("{}", row?);
    }
    Ok(())
}
