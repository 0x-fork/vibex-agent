fn main() {
    let path = std::env::args().nth(1).expect("db path");
    let mut conn = rusqlite::Connection::open(&path).expect("open");
    vibex_db::apply_migrations(&mut conn).expect("migrate");
    let ver: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    println!("schema: {ver}");
    let cols: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('agent_runtime_profiles') WHERE name IN ('models_json','modes_json','features_json','disabled_tools_json')", [], |r| r.get(0)).unwrap();
    println!("new runtime columns: {cols}/4");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_runtime_profiles WHERE deleted_at_ms IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("runtime rows: {n}");
}
