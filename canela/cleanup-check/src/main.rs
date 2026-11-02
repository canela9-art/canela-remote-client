// Prueba aislada de la lógica de src/canela_cleanup.rs: la "RustDesk Printer" solo se borra si
// RustDesk ya no está en el equipo (ni instalado ni corriendo). La parte que toca Windows no
// compila aquí (Linux): solo se prueba la decisión.
#[path = "../../../src/canela_cleanup.rs"]
mod canela_cleanup;

use canela_cleanup::*;

fn main() {
    // tasklist /fo csv /nh
    let con_rd = "\"System\",\"4\",\"Services\",\"0\",\"144 K\"\r\n\"RustDesk.exe\",\"4321\",\"Console\",\"1\",\"60,000 K\"\r\n";
    let sin_rd = "\"System\",\"4\",\"Services\",\"0\",\"144 K\"\r\n\"CanelaRemote.exe\",\"900\",\"Services\",\"0\",\"60,000 K\"\r\n\"rustdesk_helper.exe\",\"1\",\"Console\",\"1\",\"1 K\"\r\n";
    assert!(rustdesk_running(con_rd));
    assert!(!rustdesk_running(sin_rd)); // CanelaRemote.exe y otros nombres parecidos no cuentan
    assert!(!rustdesk_running(""));
    println!("tasklist       → ok");

    let nada = RustDeskSignals::default();
    assert!(should_remove(true, &nada), "sobrante: se borra");
    assert!(!should_remove(false, &nada), "sin impresora: nada que hacer");
    for s in [
        RustDeskSignals { uninstall_key: true, ..Default::default() },
        RustDeskSignals { service: true, ..Default::default() },
        RustDeskSignals { program_dir: true, ..Default::default() },
        RustDeskSignals { running: true, ..Default::default() },
    ] {
        assert!(!should_remove(true, &s), "RustDesk sigue ({s:?}): no se toca");
    }
    println!("decisión       → ok");
    assert_eq!(RUSTDESK_APP, "RustDesk"); // impresora "RustDesk Printer"
    println!("OK: canela_cleanup");
}
