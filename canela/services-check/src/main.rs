// Prueba aislada de los parsers de src/canela_services.rs con salidas de ejemplo de reg/netstat/
// tasklist de Windows (las que el agente recoge en el equipo). La detección real solo corre en
// Windows; aquí se prueba TODO lo que transforma esas salidas en el reporte.
#[path = "../../../src/canela_services.rs"]
mod canela_services;
pub use hbb_common::ResultType;
pub fn get_api_server(a: String, _c: String) -> String { a }
pub async fn post_request(_u: String, _b: String, _h: &str) -> ResultType<String> { Ok(String::new()) }

use canela_services::*;

fn main() {
    // reg query "…\Instance Names\SQL"
    let inst = "\r\nHKEY_LOCAL_MACHINE\\...\\SQL\r\n    MSSQLSERVER    REG_SZ    MSSQL16.MSSQLSERVER\r\n    SQLEXPRESS    REG_SZ    MSSQL15.SQLEXPRESS\r\n";
    let got = parse_instances(inst);
    assert_eq!(got, vec![("MSSQLSERVER".into(), "MSSQL16.MSSQLSERVER".into()), ("SQLEXPRESS".into(), "MSSQL15.SQLEXPRESS".into())]);
    println!("instancias     → {got:?}");

    // TcpDynamicPorts / TcpPort / Enabled
    assert_eq!(reg_port(parse_reg_value("    TcpDynamicPorts    REG_SZ    54213\r\n", "TcpDynamicPorts")), Some(54213));
    assert_eq!(reg_port(parse_reg_value("    TcpPort    REG_SZ    1433\r\n", "TcpPort")), Some(1433));
    assert_eq!(reg_port(parse_reg_value("    TcpDynamicPorts    REG_SZ    \r\n", "TcpDynamicPorts")), None); // vacío = sin puerto
    assert_eq!(reg_bool(parse_reg_value("    Enabled    REG_DWORD    0x1\r\n", "Enabled")), Some(true));
    assert_eq!(reg_bool(parse_reg_value("    Enabled    REG_DWORD    0x0\r\n", "Enabled")), Some(false));
    println!("valores reg    → ok");

    // netstat -ano -p tcp
    let ns = "\r\nProto  Dirección local     Dirección remota    Estado\r\n  TCP    0.0.0.0:2638     0.0.0.0:0    LISTENING    4321\r\n  TCP    0.0.0.0:1433     0.0.0.0:0    LISTENING    900\r\n  TCP    127.0.0.1:5000   0.0.0.0:0    LISTENING    111\r\n  TCP    0.0.0.0:445      1.2.3.4:52    ESTABLISHED  4\r\n";
    let listen = parse_netstat_listen(ns);
    assert_eq!(listen, vec![(2638, 4321), (1433, 900), (5000, 111)]);
    println!("netstat        → {listen:?}");

    // tasklist /fo csv /nh
    let tl = "\"dbsrv17.exe\",\"4321\",\"Services\",\"0\",\"120,000 K\"\r\n\"sqlservr.exe\",\"900\",\"Services\",\"0\",\"900,000 K\"\r\n\"otro.exe\",\"111\",\"Console\",\"1\",\"5,000 K\"\r\n";
    let procs = parse_tasklist_csv(tl);
    assert_eq!(procs.get(&4321).map(|s| s.as_str()), Some("dbsrv17.exe"));
    assert!(is_sqlanywhere("dbsrv17.exe") && is_sqlanywhere("dbeng16.exe") && !is_sqlanywhere("sqlservr.exe"));
    println!("tasklist       → ok");

    // compose: SQL Server (dinámico), SQL Anywhere (por proceso), y uno con TCP apagado
    let instances = vec![
        ("SQLEXPRESS".to_string(), "MSSQL15.SQLEXPRESS".to_string(), Some(54213u16), Some(true)),
        ("VIEJA".to_string(), "MSSQL11.VIEJA".to_string(), None, Some(false)), // TCP/IP apagado
        ("SINTCP".to_string(), "MSSQL10.SINTCP".to_string(), None, None),      // sin puerto ni dato → se omite
    ];
    let svc = compose(&instances, &listen, &procs);
    assert_eq!(svc.len(), 3, "2 SQL Server (uno apagado) + 1 SQL Anywhere");
    assert_eq!(svc[0].kind, "sqlserver"); assert_eq!(svc[0].port, 54213); assert_eq!(svc[0].tcp_enabled, Some(true));
    assert_eq!(svc[1].kind, "sqlserver"); assert_eq!(svc[1].port, 0); assert_eq!(svc[1].tcp_enabled, Some(false));
    assert_eq!(svc[2].kind, "sqlanywhere"); assert_eq!(svc[2].port, 2638);
    assert_eq!(svc[2].process.as_deref(), Some("dbsrv17.exe"));
    let arr = to_json_array(&svc);
    assert_eq!(arr[0]["instance"], "SQLEXPRESS");
    assert_eq!(arr[2]["kind"], "sqlanywhere");
    println!("compose        → {}", arr);

    // en esta plataforma (no Windows) detect() no encuentra nada
    assert!(detect().is_empty());
    println!("OK: parsers de detección de servicios");
}
