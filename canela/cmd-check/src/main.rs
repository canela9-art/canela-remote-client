// Prueba de src/canela_cmd.rs sin RustDesk: el permiso firmado en el ticket, los comandos (tiempo
// límite, salida cortada, hijos que se matan), archivos, la compresión de la captura y, de punta a
// punta, una "sesión" (par TCP local) con varios pedidos seguidos. La captura es de mentira: scrap
// no compila aquí (canela_cmd_capture.rs se prueba en el build de verdad).
#[path = "../../../src/canela_ticket.rs"]
mod canela_ticket;
#[path = "../../../src/canela_socks.rs"]
mod canela_socks;
#[path = "../../../src/canela_cmd.rs"]
mod canela_cmd;

mod canela_cmd_capture {
    use crate::canela_cmd::Shot;
    pub fn displays() -> usize {
        1
    }
    /// Una pantalla de 2400×300 en degradado, como la mandaría scrap ya en RGBA.
    pub fn capture(idx: usize) -> Result<Shot, String> {
        if idx > 0 {
            return Err(format!("no hay pantalla {idx}: este equipo tiene 1 (se cuentan desde 0)"));
        }
        let (w, h) = (2400u32, 300u32);
        let rgba = (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, 80, 160, 255]).collect();
        Ok(Shot { width: w, height: h, rgba, displays: 1 })
    }
}

pub use hbb_common::ResultType;
pub fn get_api_server(a: String, _c: String) -> String {
    a
}
pub async fn post_request(_u: String, _b: String, _h: &str) -> ResultType<String> {
    Ok(String::new())
}
pub fn decode64<T: AsRef<[u8]>>(i: T) -> Result<Vec<u8>, hbb_common::base64::DecodeError> {
    use hbb_common::base64::Engine;
    hbb_common::base64::engine::general_purpose::STANDARD.decode(i)
}

use canela_cmd::*;
use hbb_common::base64::{engine::general_purpose::STANDARD as B64, Engine};
use hbb_common::tokio::{
    self,
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn req(v: Value) -> Value {
    handle(v.to_string().as_bytes(), "dueño")
}

fn error_of(v: Value) -> String {
    v["error"].as_str().unwrap_or_default().to_owned()
}

/// Solo un ticket válido de la API con "cmd": true abre los comandos.
fn permiso() {
    use hbb_common::sodiumoxide::crypto::sign;
    let (pk, sk) = sign::gen_keypair();
    let me = hbb_common::config::Config::get_id();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let lr = |claims: Option<Value>, key: &sign::SecretKey| {
        let mut lr = hbb_common::message_proto::LoginRequest { my_id: "555".into(), ..Default::default() };
        if let Some(c) = claims {
            let t = B64.encode(sign::sign(c.to_string().as_bytes(), key));
            lr.special_fields.mut_unknown_fields().add_length_delimited(canela_ticket::FIELD, t.into_bytes());
        }
        lr
    };
    let base = json!({"v":1,"to":me,"from":"555","u":"dueno","iat":now,"exp":now + 600});
    let mut con_cmd = base.clone();
    con_cmd["cmd"] = json!(true);

    // sin llave de tickets en el equipo: nunca, aunque el ticket diga cmd
    hbb_common::config::Config::set_option(canela_ticket::OPTION_PK.into(), String::new());
    assert!(allowed(&lr(Some(con_cmd.clone()), &sk)).is_err(), "sin llave no hay comandos");

    hbb_common::config::Config::set_option(canela_ticket::OPTION_PK.into(), B64.encode(pk.0));
    assert_eq!(allowed(&lr(Some(con_cmd.clone()), &sk)), Ok("dueno".into()), "ticket del dueño");
    assert!(canela_ticket::verify(&lr(Some(con_cmd.clone()), &sk)).is_ok(), "verify sigue igual");
    assert!(canela_ticket::verify(&lr(Some(base.clone()), &sk)).is_ok(), "el técnico sigue entrando");
    assert!(allowed(&lr(Some(base.clone()), &sk)).unwrap_err().contains("permiso"), "ticket de técnico");
    assert!(allowed(&lr(None, &sk)).is_err(), "sin ticket");
    let (_, otra) = sign::gen_keypair();
    assert!(allowed(&lr(Some(con_cmd.clone()), &otra)).is_err(), "firma falsa");
    let mut texto = con_cmd.clone();
    texto["cmd"] = json!("true");
    assert!(allowed(&lr(Some(texto), &sk)).is_err(), "cmd tiene que ser true de verdad");
    let mut vencido = con_cmd.clone();
    vencido["exp"] = json!(now - 1000);
    assert!(allowed(&lr(Some(vencido), &sk)).is_err(), "vencido");
    let mut otro_equipo = con_cmd.clone();
    otro_equipo["to"] = json!("999");
    assert!(allowed(&lr(Some(otro_equipo), &sk)).is_err(), "ticket de otro equipo");
    hbb_common::config::Config::set_option(OPTION.into(), "N".into());
    assert!(allowed(&lr(Some(con_cmd), &sk)).unwrap_err().contains("canela-cmd=N"), "apagado en el equipo");
    hbb_common::config::Config::set_option(OPTION.into(), String::new());
    println!("permiso        → ok (solo ticket firmado con cmd: true, para este equipo, vigente, no apagado)");
}

fn comandos() {
    let r = req(json!({"op":"exec","cmd":"echo hola; echo error 1>&2; exit 3"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["stdout"], "hola\n");
    assert_eq!(r["stderr"], "error\n");
    assert_eq!(r["code"], 3);
    assert_eq!(r["timed_out"], false);

    let r = req(json!({"op":"exec","cmd":"printf 'acentos: áéíóú ñ'"}));
    assert_eq!(r["stdout"], "acentos: áéíóú ñ");

    let r = req(json!({"op":"exec","cmd":"pwd","cwd":"/tmp"}));
    assert!(r["stdout"].as_str().unwrap().trim_end().ends_with("tmp"), "{r}");
    assert!(error_of(req(json!({"op":"exec","cmd":"pwd","cwd":"/no/existe"}))).contains("no existe"));
    assert!(error_of(req(json!({"op":"exec","cmd":"  "}))).contains("falta"));
    assert!(error_of(req(json!({"op":"exec","cmd":"x","shell":"bash"}))).contains("shell desconocido"));

    // vencido: se mata, con el nieto que tiene el tubo abierto, y vuelve enseguida
    let t = Instant::now();
    let r = req(json!({"op":"exec","cmd":"echo antes; sleep 30 & sleep 30","timeout":1}));
    assert_eq!(r["timed_out"], true, "{r}");
    assert_eq!(r["stdout"], "antes\n");
    assert!(t.elapsed() < Duration::from_secs(5), "tardó {:?}", t.elapsed());

    // salida enorme: se corta en MAX_OUTPUT y lo dice
    let r = req(json!({"op":"exec","cmd":"head -c 3000000 /dev/zero | tr '\\0' a"}));
    assert_eq!(r["stdout"].as_str().unwrap().len(), MAX_OUTPUT);
    assert_eq!(r["truncated"], true);

    // PowerShell va en -EncodedCommand: UTF-16LE en base64, sin líos de comillas
    let enc = B64.decode(powershell_encoded("Write-Output \"a'b\"")).unwrap();
    let u16s: Vec<u16> = enc.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    assert!(String::from_utf16(&u16s).unwrap().ends_with("Write-Output \"a'b\""));
    println!("comandos       → ok (salida, código, acentos, carpeta, tiempo límite, corte)");
}

fn archivos() {
    let dir = std::env::temp_dir().join(format!("canela-cmd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("prueba.txt");
    let data = "línea 1\nlínea 2\n".as_bytes();
    let r = req(json!({"op":"write_file","path":f.to_str().unwrap(),"b64":B64.encode(data)}));
    assert_eq!(r["ok"], true, "{r}");
    let r = req(json!({"op":"read_file","path":f.to_str().unwrap()}));
    assert_eq!(B64.decode(r["b64"].as_str().unwrap()).unwrap(), data);
    assert!(error_of(req(json!({"op":"read_file","path":dir.to_str().unwrap()}))).contains("carpeta"));
    assert_eq!(req(json!({"op":"read_file","path":"/no/existe.txt"}))["ok"], false);
    assert!(error_of(req(json!({"op":"write_file","path":f.to_str().unwrap(),"b64":"%%%"}))).contains("base64"));
    let grande = dir.join("grande.bin");
    std::fs::File::create(&grande).unwrap().set_len(MAX_FILE + 1).unwrap();
    assert!(error_of(req(json!({"op":"read_file","path":grande.to_str().unwrap()}))).contains("20 MB"));
    std::fs::remove_dir_all(&dir).unwrap();
    println!("archivos       → ok (ida y vuelta, carpeta, no existe, base64 malo, máx. 20 MB)");
}

fn pantalla() {
    // fila con relleno (stride 12 para 2 píxeles) y BGRA → RGBA compacto
    let data = [1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, 7, 8, 9, 0, 10, 11, 12, 0, 9, 9, 9, 9];
    assert_eq!(
        to_rgba(&data, 2, 2, 12, true).unwrap(),
        vec![3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
    );
    assert!(to_rgba(&data[..19], 2, 2, 12, true).is_err(), "incompleta");

    let r = req(json!({"op":"screenshot","max_width":1200}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(
        (r["width"].as_u64(), r["sent_width"].as_u64(), r["sent_height"].as_u64()),
        (Some(2400), Some(1200), Some(150))
    );
    let jpeg = B64.decode(r["jpeg_b64"].as_str().unwrap()).unwrap();
    let img = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
    assert_eq!((img.width(), img.height()), (1200, 150));
    assert_eq!(req(json!({"op":"screenshot","max_width":99999}))["sent_width"], 2400, "no se agranda");
    assert!(error_of(req(json!({"op":"screenshot","display":3}))).contains("tiene 1"));
    println!("pantalla       → ok (stride, BGRA, JPEG achicado a {}×{} = {} KB)", img.width(), img.height(), jpeg.len() / 1024);
}

fn pedidos_malos() {
    assert!(error_of(handle(b"no es json", "x")).contains("JSON"));
    assert!(error_of(req(json!({"op":"formatear"}))).contains("desconocida"));
    let i = req(json!({"op":"info"}));
    assert_eq!((i["ok"].as_bool(), i["displays"].as_u64()), (Some(true), Some(1)));
    println!("pedidos        → ok (JSON malo, operación desconocida, info)");
}

/// La sesión de punta a punta: varios pedidos en orden por la misma conexión.
async fn sesion() {
    let (near, far) = canela_socks::loopback_pair().await.unwrap();
    tokio::spawn(serve(far, "dueño".into()));
    let (rd, mut wr) = near.into_split();
    let mut rd = BufReader::new(rd);
    for word in ["uno", "dos", "tres"] {
        let line = format!("{}\n", json!({"op":"exec","cmd":format!("echo {word}")}));
        wr.write_all(line.as_bytes()).await.unwrap();
        let mut resp = String::new();
        rd.read_line(&mut resp).await.unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["stdout"], format!("{word}\n"));
    }
    // una línea de más de MAX_LINE: error y se cierra
    let big = vec![b'a'; MAX_LINE + 10];
    let writer = tokio::spawn(async move {
        let _ = wr.write_all(&big).await;
    });
    let mut resp = String::new();
    rd.read_line(&mut resp).await.unwrap();
    assert!(resp.contains("demasiado grande"), "{resp}");
    writer.abort();
    println!("sesión         → ok (3 pedidos seguidos por la misma sesión, línea gigante cortada)");
}

/// `cargo run -- serve <ID> <puerto>`: atiende el protocolo de verdad en 0.0.0.0:<puerto> como el
/// equipo <ID>, para probar el MCP (mcp/test/smoke-pc.mjs) contra este código sin la sesión.
async fn serve_for_mcp(id: &str, port: u16) {
    hbb_common::config::Config::set_id(id);
    let l = tokio::net::TcpListener::bind(("0.0.0.0", port)).await.unwrap();
    println!("escuchando como {id} en {port}");
    loop {
        let (s, _) = l.accept().await.unwrap();
        tokio::spawn(serve(s, "dueño".into()));
    }
}

fn main() {
    *hbb_common::config::APP_NAME.write().unwrap() = "CanelaCmdTest".into();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("serve") {
        let port = args[3].parse().unwrap();
        return tokio::runtime::Runtime::new().unwrap().block_on(serve_for_mcp(&args[2], port));
    }
    permiso();
    comandos();
    archivos();
    pantalla();
    pedidos_malos();
    tokio::runtime::Runtime::new().unwrap().block_on(sesion());
    println!("OK: comandos del dueño");
}
