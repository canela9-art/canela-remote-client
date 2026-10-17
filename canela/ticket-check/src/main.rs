#[path = "../../../src/canela_ticket.rs"]
mod canela_ticket;
pub use hbb_common::ResultType;
pub fn get_api_server(a: String, _c: String) -> String { a }
pub async fn post_request(_u: String, _b: String, _h: &str) -> ResultType<String> { Ok(String::new()) }
pub fn decode64<T: AsRef<[u8]>>(i: T) -> Result<Vec<u8>, hbb_common::base64::DecodeError> {
    use hbb_common::base64::Engine; hbb_common::base64::engine::general_purpose::STANDARD.decode(i) }
fn main() {
    *hbb_common::config::APP_NAME.write().unwrap() = "CanelaTicketTest".into();
    use hbb_common::sodiumoxide::crypto::sign;
    use hbb_common::base64::Engine;
    let (pk, sk) = sign::gen_keypair();
    let b64 = |b: &[u8]| hbb_common::base64::engine::general_purpose::STANDARD.encode(b);
    hbb_common::config::Config::set_option(canela_ticket::OPTION_PK.into(), b64(&pk.0));
    let me = hbb_common::config::Config::get_id();
    let mk = |to: &str, from: &str, exp: i64| {
        let p = serde_json::json!({"v":1,"to":to,"from":from,"u":"tec","exp":exp}).to_string();
        b64(&sign::sign(p.as_bytes(), &sk)) };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let lr_with = |t: Option<String>| { let mut lr = hbb_common::message_proto::LoginRequest { my_id: "555".into(), ..Default::default() };
        if let Some(t) = t { lr.special_fields.mut_unknown_fields().add_length_delimited(canela_ticket::FIELD, t.into_bytes()); }
        // simula la red: serializa y vuelve a parsear
        use hbb_common::protobuf::Message; let b = lr.write_to_bytes().unwrap(); hbb_common::message_proto::LoginRequest::parse_from_bytes(&b).unwrap() };
    let check = |name: &str, r: Result<(), String>, ok: bool| { println!("{name:<14} → {r:?}"); assert_eq!(r.is_ok(), ok, "{name}"); };
    check("sin ticket", canela_ticket::verify(&lr_with(None)), false);
    check("válido", canela_ticket::verify(&lr_with(Some(mk(&me, "555", now + 600)))), true);
    check("otro equipo", canela_ticket::verify(&lr_with(Some(mk("999", "555", now + 600)))), false);
    check("otro técnico", canela_ticket::verify(&lr_with(Some(mk(&me, "777", now + 600)))), false);
    check("vencido", canela_ticket::verify(&lr_with(Some(mk(&me, "555", now - 1000)))), false);
    let (_pk2, sk2) = sign::gen_keypair();
    let forged = b64(&sign::sign(serde_json::json!({"v":1,"to":me,"from":"555","exp":now+600}).to_string().as_bytes(), &sk2));
    check("firma falsa", canela_ticket::verify(&lr_with(Some(forged))), false);
    println!("OK: todos los casos");

    // parche 28: motivo exacto para el técnico y hora del servidor en vez del reloj del equipo
    let why = |t: Option<String>| canela_ticket::verify(&lr_with(t)).unwrap_err();
    assert!(why(None).contains("sesión iniciada"), "sin ticket");
    let mut p = serde_json::json!({"v":1,"to":me,"from":"555","u":"tec","iat":now - 3600,"exp":now - 3000});
    let adelantado = why(Some(b64(&sign::sign(p.to_string().as_bytes(), &sk))));
    assert!(adelantado.contains("60 min adelantada"), "sin hora del servidor: {adelantado}");
    // el equipo va 1 h adelantado: con la hora del servidor el mismo ticket entra
    canela_ticket::set_server_time(now - 3600);
    check("reloj +1 h", canela_ticket::verify(&lr_with(Some(b64(&sign::sign(p.to_string().as_bytes(), &sk))))), true);
    p["iat"] = (now - 3600 - 2000).into();
    p["exp"] = (now - 3600 - 1400).into();
    let vencido = why(Some(b64(&sign::sign(p.to_string().as_bytes(), &sk))));
    assert!(vencido.contains("vuelve a conectar"), "vencido de verdad: {vencido}");
    assert!(why(Some(mk("999", "555", now + 600))).contains("equipo 999"), "otro equipo");
    canela_ticket::set_server_time(now);
    println!("hora servidor  → ok (motivos claros, reloj del equipo ignorado)");

    // parche 15: la clave fija viaja con el ticket y se entrega una sola vez
    canela_ticket::store("111222333", r#"{"ticket":"abc","exp":1,"password":"Fija-123"}"#);
    assert_eq!(canela_ticket::take_password("111222333").as_deref(), Some("Fija-123"), "clave con el ticket");
    assert_eq!(canela_ticket::take_password("111222333"), None, "se entrega una sola vez");
    canela_ticket::store("111222333", r#"{"ticket":"abc","exp":1}"#);
    assert_eq!(canela_ticket::take_password("111222333"), None, "sin clave fija no hay clave");
    canela_ticket::store("111222333", r#"{"ticket":"abc","exp":1,"password":"Vieja"}"#);
    canela_ticket::store("111222333", r#"{"error":"No tienes acceso a este equipo"}"#);
    assert_eq!(canela_ticket::take_password("111222333"), None, "un pedido negado borra la clave anterior");
    canela_ticket::store("444555666", r#"{"ticket":"abc","exp":1,"password":"Con-Servidor"}"#);
    assert_eq!(canela_ticket::take_password("444555666@otro"), Some("Con-Servidor".into()), "id@servidor");
    println!("clave fija     → ok (con ticket, una vez, se borra al negar)");
}
