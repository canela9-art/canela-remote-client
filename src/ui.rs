use std::{
    collections::HashMap,
    iter::FromIterator,
    sync::{Arc, Mutex},
};

use sciter::Value;

use hbb_common::{
    allow_err,
    config::{LocalConfig, PeerConfig},
    log,
};

#[cfg(not(feature = "flutter"))]
use crate::ui_session_interface::Session;
use crate::{common::get_app_name, ipc, ui_interface::*};

mod cm;
#[cfg(feature = "inline")]
pub mod inline;
pub mod remote;

#[allow(dead_code)]
type Status = (i32, bool, i64, String);

lazy_static::lazy_static! {
    // stupid workaround for https://sciter.com/forums/topic/crash-on-latest-tis-mac-sdk-sometimes/
    static ref STUPID_VALUES: Mutex<Vec<Arc<Vec<Value>>>> = Default::default();
}

#[cfg(not(feature = "flutter"))]
lazy_static::lazy_static! {
    pub static ref CUR_SESSION: Arc<Mutex<Option<Session<remote::SciterHandler>>>> = Default::default();
}

struct UIHostHandler;

pub fn start(args: &mut [String]) {
    #[cfg(target_os = "macos")]
    crate::platform::delegate::show_dock();
    #[cfg(all(target_os = "linux", feature = "inline"))]
    {
        let app_dir = std::env::var("APPDIR").unwrap_or("".to_string());
        let mut so_path = "/usr/share/rustdesk/libsciter-gtk.so".to_owned();
        for (prefix, dir) in [
            ("", "/usr"),
            ("", "/app"),
            (&app_dir, "/usr"),
            (&app_dir, "/app"),
        ]
        .iter()
        {
            let path = format!("{prefix}{dir}/share/rustdesk/libsciter-gtk.so");
            if std::path::Path::new(&path).exists() {
                so_path = path;
                break;
            }
        }
        sciter::set_library(&so_path).ok();
    }
    #[cfg(windows)]
    // Check if there is a sciter.dll nearby.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sciter_dll_path = parent.join("sciter.dll");
            if sciter_dll_path.exists() {
                // Try to set the sciter dll.
                let p = sciter_dll_path.to_string_lossy().to_string();
                log::debug!("Found dll:{}, \n {:?}", p, sciter::set_library(&p));
            }
        }
    }
    // https://github.com/c-smile/sciter-sdk/blob/master/include/sciter-x-types.h
    // https://github.com/rustdesk/rustdesk/issues/132#issuecomment-886069737
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::GfxLayer(
        sciter::GFX_LAYER::WARP
    )));
    use sciter::SCRIPT_RUNTIME_FEATURES::*;
    allow_err!(sciter::set_options(sciter::RuntimeOptions::ScriptFeatures(
        ALLOW_FILE_IO as u8 | ALLOW_SOCKET_IO as u8 | ALLOW_EVAL as u8 | ALLOW_SYSINFO as u8
    )));
    let mut frame = sciter::WindowBuilder::main_window().create();
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::UxTheming(true)));
    frame.set_title(&crate::get_app_name());
    #[cfg(target_os = "macos")]
    crate::platform::delegate::make_menubar(frame.get_host(), args.is_empty());
    #[cfg(windows)]
    crate::platform::try_set_window_foreground(frame.get_hwnd() as _);
    let page;
    if args.len() > 1 && args[0] == "--play" {
        args[0] = "--connect".to_owned();
        let path: std::path::PathBuf = (&args[1]).into();
        let id = path
            .file_stem()
            .map(|p| p.to_str().unwrap_or(""))
            .unwrap_or("")
            .to_owned();
        args[1] = id;
    }
    if args.is_empty() {
        std::thread::spawn(move || check_zombie());
        crate::common::check_software_update();
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "index.html";
        // Start pulse audio local server.
        #[cfg(target_os = "linux")]
        std::thread::spawn(crate::ipc::start_pa);
    } else if args[0] == "--install" {
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "install.html";
    } else if args[0] == "--cm" {
        frame.register_behavior("connection-manager", move || {
            Box::new(cm::SciterConnectionManager::new())
        });
        page = "cm.html";
        *cm::HIDE_CM.lock().unwrap() = crate::ipc::get_config("hide_cm")
            .ok()
            .flatten()
            .unwrap_or_default()
            == "true";
    } else if (args[0] == "--connect"
        || args[0] == "--file-transfer"
        || args[0] == "--port-forward"
        || args[0] == "--rdp")
        && args.len() > 1
    {
        #[cfg(windows)]
        {
            let hw = frame.get_host().get_hwnd();
            crate::platform::windows::enable_lowlevel_keyboard(hw as _);
        }
        let mut iter = args.iter();
        let Some(cmd) = iter.next() else {
            log::error!("Failed to get cmd arg");
            return;
        };
        let cmd = cmd.to_owned();
        let Some(id) = iter.next() else {
            log::error!("Failed to get id arg");
            return;
        };
        let id = id.to_owned();
        let pass = iter.next().unwrap_or(&"".to_owned()).clone();
        let args: Vec<String> = iter.map(|x| x.clone()).collect();
        frame.set_title(&id);
        frame.register_behavior("native-remote", move || {
            let handler =
                remote::SciterSession::new(cmd.clone(), id.clone(), pass.clone(), args.clone());
            #[cfg(not(feature = "flutter"))]
            {
                *CUR_SESSION.lock().unwrap() = Some(handler.inner());
            }
            Box::new(handler)
        });
        page = "remote.html";
    } else {
        log::error!("Wrong command: {:?}", args);
        return;
    }
    #[cfg(feature = "inline")]
    {
        let html = if page == "index.html" {
            inline::get_index()
        } else if page == "cm.html" {
            inline::get_cm()
        } else if page == "install.html" {
            inline::get_install()
        } else {
            inline::get_remote()
        };
        frame.load_html(html.as_bytes(), Some(page));
    }
    #[cfg(not(feature = "inline"))]
    frame.load_file(&format!(
        "file://{}/src/ui/{}",
        std::env::current_dir()
            .map(|c| c.display().to_string())
            .unwrap_or("".to_owned()),
        page
    ));
    let hide_cm = *cm::HIDE_CM.lock().unwrap();
    if !args.is_empty() && args[0] == "--cm" && hide_cm {
        // run_app calls expand(show) + run_loop, we use collapse(hide) + run_loop instead to create a hidden window
        frame.collapse(true);
        frame.run_loop();
        return;
    }
    frame.run_app();
}

struct UI {}

impl UI {
    fn recent_sessions_updated(&self) -> bool {
        recent_sessions_updated()
    }

    fn get_id(&self) -> String {
        ipc::get_id()
    }

    fn temporary_password(&mut self) -> String {
        temporary_password()
    }

    fn update_temporary_password(&self) {
        update_temporary_password()
    }

    fn set_permanent_password(&self, password: String) {
        let _ = set_permanent_password_with_result(password);
    }

    fn is_local_permanent_password_set(&self) -> bool {
        is_local_permanent_password_set()
    }

    fn is_permanent_password_set(&self) -> bool {
        is_permanent_password_set()
    }

    fn get_remote_id(&mut self) -> String {
        LocalConfig::get_remote_id()
    }

    fn set_remote_id(&mut self, id: String) {
        LocalConfig::set_remote_id(&id);
    }

    fn goto_install(&mut self) {
        goto_install();
    }

    fn install_me(&mut self, _options: String, _path: String) {
        install_me(_options, _path, false, false);
    }

    fn update_me(&self, _path: String) {
        update_me(_path);
    }

    fn run_without_install(&self) {
        run_without_install();
    }

    fn show_run_without_install(&self) -> bool {
        show_run_without_install()
    }

    fn get_license(&self) -> String {
        get_license()
    }

    fn get_option(&self, key: String) -> String {
        get_option(key)
    }

    fn get_local_option(&self, key: String) -> String {
        get_local_option(key)
    }

    fn set_local_option(&self, key: String, value: String) {
        set_local_option(key, value);
    }

    fn peer_has_password(&self, id: String) -> bool {
        peer_has_password(id)
    }

    fn forget_password(&self, id: String) {
        forget_password(id)
    }

    fn get_peer_option(&self, id: String, name: String) -> String {
        get_peer_option(id, name)
    }

    fn set_peer_option(&self, id: String, name: String, value: String) {
        set_peer_option(id, name, value)
    }

    fn using_public_server(&self) -> bool {
        crate::using_public_server()
    }

    fn is_incoming_only(&self) -> bool {
        hbb_common::config::is_incoming_only()
    }

    pub fn is_outgoing_only(&self) -> bool {
        hbb_common::config::is_outgoing_only()
    }

    pub fn is_custom_client(&self) -> bool {
        crate::common::is_custom_client()
    }

    pub fn is_disable_settings(&self) -> bool {
        hbb_common::config::is_disable_settings()
    }

    pub fn is_disable_account(&self) -> bool {
        hbb_common::config::is_disable_account()
    }

    pub fn is_disable_installation(&self) -> bool {
        hbb_common::config::is_disable_installation()
    }

    pub fn is_disable_ab(&self) -> bool {
        hbb_common::config::is_disable_ab()
    }

    fn get_options(&self) -> Value {
        let hashmap: HashMap<String, String> =
            serde_json::from_str(&get_options()).unwrap_or_default();
        let mut m = Value::map();
        for (k, v) in hashmap {
            m.set_item(k, v);
        }
        m
    }

    fn test_if_valid_server(&self, host: String, test_with_proxy: bool) -> String {
        test_if_valid_server(host, test_with_proxy)
    }

    fn get_sound_inputs(&self) -> Value {
        Value::from_iter(get_sound_inputs())
    }

    fn set_options(&self, v: Value) {
        let mut m = HashMap::new();
        for (k, v) in v.items() {
            if let Some(k) = k.as_string() {
                if let Some(v) = v.as_string() {
                    if !v.is_empty() {
                        m.insert(k, v);
                    }
                }
            }
        }
        set_options(m);
    }

    fn set_option(&self, key: String, value: String) {
        set_option(key, value);
    }

    fn install_path(&mut self) -> String {
        install_path()
    }

    fn install_options(&self) -> String {
        install_options()
    }

    fn get_socks(&self) -> Value {
        Value::from_iter(get_socks())
    }

    fn set_socks(&self, proxy: String, username: String, password: String) {
        set_socks(proxy, username, password)
    }

    fn is_installed(&self) -> bool {
        is_installed()
    }

    fn get_supported_privacy_mode_impls(&self) -> String {
        serde_json::to_string(&crate::privacy_mode::get_supported_privacy_mode_impl())
            .unwrap_or_default()
    }

    fn is_root(&self) -> bool {
        is_root()
    }

    fn is_release(&self) -> bool {
        #[cfg(not(debug_assertions))]
        return true;
        #[cfg(debug_assertions)]
        return false;
    }

    fn is_share_rdp(&self) -> bool {
        is_share_rdp()
    }

    fn set_share_rdp(&self, _enable: bool) {
        set_share_rdp(_enable);
    }

    fn is_installed_lower_version(&self) -> bool {
        is_installed_lower_version()
    }

    fn closing(&mut self, x: i32, y: i32, w: i32, h: i32) {
        crate::server::input_service::fix_key_down_timeout_at_exit();
        LocalConfig::set_size(x, y, w, h);
    }

    fn get_size(&mut self) -> Value {
        let s = LocalConfig::get_size();
        let mut v = Vec::new();
        v.push(s.0);
        v.push(s.1);
        v.push(s.2);
        v.push(s.3);
        Value::from_iter(v)
    }

    fn get_mouse_time(&self) -> f64 {
        get_mouse_time()
    }

    fn check_mouse_time(&self) {
        check_mouse_time()
    }

    fn get_connect_status(&mut self) -> Value {
        let mut v = Value::array(0);
        let x = get_connect_status();
        v.push(x.status_num);
        v.push(x.key_confirmed);
        v.push(x.id);
        v
    }

    #[inline]
    fn get_peer_value(id: String, p: PeerConfig) -> Value {
        let values = vec![
            id,
            p.info.username.clone(),
            p.info.hostname.clone(),
            p.info.platform.clone(),
            p.options.get("alias").unwrap_or(&"".to_owned()).to_owned(),
        ];
        Value::from_iter(values)
    }

    fn get_peer(&self, id: String) -> Value {
        let c = get_peer(id.clone());
        Self::get_peer_value(id, c)
    }

    fn get_fav(&self) -> Value {
        Value::from_iter(get_fav())
    }

    fn store_fav(&self, fav: Value) {
        let mut tmp = vec![];
        fav.values().for_each(|v| {
            if let Some(v) = v.as_string() {
                if !v.is_empty() {
                    tmp.push(v);
                }
            }
        });
        store_fav(tmp);
    }

    fn get_recent_sessions(&mut self) -> Value {
        // to-do: limit number of recent sessions, and remove old peer file
        let peers: Vec<Value> = PeerConfig::peers(None)
            .drain(..)
            .map(|p| Self::get_peer_value(p.0, p.2))
            .collect();
        Value::from_iter(peers)
    }

    fn get_icon(&mut self) -> String {
        get_icon()
    }

    fn remove_peer(&mut self, id: String) {
        PeerConfig::remove(&id);
    }

    fn remove_discovered(&mut self, id: String) {
        remove_discovered(id);
    }

    fn send_wol(&mut self, id: String) {
        crate::lan::send_wol(id)
    }

    fn new_remote(&mut self, id: String, remote_type: String, force_relay: bool) {
        new_remote(id, remote_type, force_relay)
    }

    fn is_process_trusted(&mut self, _prompt: bool) -> bool {
        is_process_trusted(_prompt)
    }

    fn is_can_screen_recording(&mut self, _prompt: bool) -> bool {
        is_can_screen_recording(_prompt)
    }

    fn is_installed_daemon(&mut self, _prompt: bool) -> bool {
        is_installed_daemon(_prompt)
    }

    fn get_error(&mut self) -> String {
        get_error()
    }

    fn is_login_wayland(&mut self) -> bool {
        is_login_wayland()
    }

    fn current_is_wayland(&mut self) -> bool {
        current_is_wayland()
    }

    fn get_software_update_url(&self) -> String {
        crate::SOFTWARE_UPDATE_URL.lock().unwrap().clone()
    }

    fn get_new_version(&self) -> String {
        get_new_version()
    }

    fn get_version(&self) -> String {
        get_version()
    }

    fn get_fingerprint(&self) -> String {
        get_fingerprint()
    }

    fn get_app_name(&self) -> String {
        get_app_name()
    }

    fn get_software_ext(&self) -> String {
        #[cfg(windows)]
        let p = "exe";
        #[cfg(target_os = "macos")]
        let p = "dmg";
        #[cfg(target_os = "linux")]
        let p = "deb";
        p.to_owned()
    }

    fn get_software_store_path(&self) -> String {
        let mut p = std::env::temp_dir();
        let name = crate::SOFTWARE_UPDATE_URL
            .lock()
            .unwrap()
            .split("/")
            .last()
            .map(|x| x.to_owned())
            .unwrap_or(crate::get_app_name());
        p.push(name);
        format!("{}.{}", p.to_string_lossy(), self.get_software_ext())
    }

    fn create_shortcut(&self, _id: String) {
        #[cfg(windows)]
        create_shortcut(_id)
    }

    fn discover(&self) {
        std::thread::spawn(move || {
            allow_err!(crate::lan::discover());
        });
    }

    fn get_lan_peers(&self) -> String {
        // let peers = get_lan_peers()
        //     .into_iter()
        //     .map(|mut peer| {
        //         (
        //             peer.remove("id").unwrap_or_default(),
        //             peer.remove("username").unwrap_or_default(),
        //             peer.remove("hostname").unwrap_or_default(),
        //             peer.remove("platform").unwrap_or_default(),
        //         )
        //     })
        //     .collect::<Vec<(String, String, String, String)>>();
        serde_json::to_string(&get_lan_peers()).unwrap_or_default()
    }

    fn get_uuid(&self) -> String {
        get_uuid()
    }

    fn open_url(&self, url: String) {
        #[cfg(windows)]
        let p = "explorer";
        #[cfg(target_os = "macos")]
        let p = "open";
        #[cfg(target_os = "linux")]
        let p = if std::path::Path::new("/usr/bin/firefox").exists() {
            "firefox"
        } else {
            "xdg-open"
        };
        allow_err!(std::process::Command::new(p).arg(url).spawn());
    }

    fn change_id(&self, id: String) {
        reset_async_job_status();
        let old_id = self.get_id();
        change_id_shared(id, old_id);
    }

    fn http_request(&self, url: String, method: String, body: Option<String>, header: String) {
        http_request(url, method, body, header)
    }

    fn post_request(&self, url: String, body: String, header: String) {
        post_request(url, body, header)
    }

    fn is_ok_change_id(&self) -> bool {
        hbb_common::machine_uid::get().is_ok()
    }

    fn get_async_job_status(&self) -> String {
        get_async_job_status()
    }

    fn get_http_status(&self, url: String) -> Option<String> {
        get_async_http_status(url)
    }

    fn t(&self, name: String) -> String {
        crate::client::translate(name)
    }

    fn is_xfce(&self) -> bool {
        crate::platform::is_xfce()
    }

    fn get_api_server(&self) -> String {
        get_api_server()
    }

    fn has_hwcodec(&self) -> bool {
        has_hwcodec()
    }

    fn has_vram(&self) -> bool {
        has_vram()
    }

    fn get_langs(&self) -> String {
        get_langs()
    }

    fn video_save_directory(&self, root: bool) -> String {
        video_save_directory(root)
    }

    fn handle_relay_id(&self, id: String) -> String {
        handle_relay_id(&id).to_owned()
    }

    fn get_login_device_info(&self) -> String {
        get_login_device_info_json()
    }

    fn support_remove_wallpaper(&self) -> bool {
        support_remove_wallpaper()
    }

    fn has_valid_2fa(&self) -> bool {
        has_valid_2fa()
    }

    fn generate2fa(&self) -> String {
        generate2fa()
    }

    pub fn verify2fa(&self, code: String) -> bool {
        verify2fa(code)
    }

    fn verify_login(&self, raw: String, id: String) -> bool {
        crate::verify_login(&raw, &id)
    }

    fn generate_2fa_img_src(&self, data: String) -> String {
        let v = qrcode_generator::to_png_to_vec(data, qrcode_generator::QrCodeEcc::Low, 128)
            .unwrap_or_default();
        let s = hbb_common::sodiumoxide::base64::encode(
            v,
            hbb_common::sodiumoxide::base64::Variant::Original,
        );
        format!("data:image/png;base64,{s}")
    }

    pub fn check_hwcodec(&self) {
        check_hwcodec()
    }

    fn is_option_fixed(&self, key: String) -> bool {
        crate::ui_interface::is_option_fixed(&key)
    }

    fn get_builtin_option(&self, key: String) -> String {
        crate::ui_interface::get_builtin_option(&key)
    }

    fn is_remote_modify_enabled_by_control_permissions(&self) -> String {
        match crate::ui_interface::is_remote_modify_enabled_by_control_permissions() {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        }
        .to_string()
    }
}

impl sciter::EventHandler for UI {
    sciter::dispatch_script_call! {
        fn t(String);
        fn get_api_server();
        fn is_xfce();
        fn using_public_server();
        fn is_custom_client();
        fn is_outgoing_only();
        fn is_incoming_only();
        fn is_disable_settings();
        fn is_disable_account();
        fn is_disable_installation();
        fn is_disable_ab();
        fn get_id();
        fn temporary_password();
        fn update_temporary_password();
        fn set_permanent_password(String);
        fn is_local_permanent_password_set();
        fn is_permanent_password_set();
        fn get_remote_id();
        fn set_remote_id(String);
        fn closing(i32, i32, i32, i32);
        fn get_size();
        fn new_remote(String, String, bool);
        fn send_wol(String);
        fn remove_peer(String);
        fn remove_discovered(String);
        fn get_connect_status();
        fn get_mouse_time();
        fn check_mouse_time();
        fn get_recent_sessions();
        fn get_peer(String);
        fn get_fav();
        fn store_fav(Value);
        fn recent_sessions_updated();
        fn get_icon();
        fn install_me(String, String);
        fn is_installed();
        fn get_supported_privacy_mode_impls();
        fn is_root();
        fn is_release();
        fn set_socks(String, String, String);
        fn get_socks();
        fn is_share_rdp();
        fn set_share_rdp(bool);
        fn is_installed_lower_version();
        fn install_path();
        fn install_options();
        fn goto_install();
        fn is_process_trusted(bool);
        fn is_can_screen_recording(bool);
        fn is_installed_daemon(bool);
        fn get_error();
        fn is_login_wayland();
        fn current_is_wayland();
        fn get_options();
        fn get_option(String);
        fn get_local_option(String);
        fn set_local_option(String, String);
        fn get_peer_option(String, String);
        fn peer_has_password(String);
        fn forget_password(String);
        fn set_peer_option(String, String, String);
        fn get_license();
        fn test_if_valid_server(String, bool);
        fn get_sound_inputs();
        fn set_options(Value);
        fn set_option(String, String);
        fn get_software_update_url();
        fn get_new_version();
        fn get_version();
        fn get_fingerprint();
        fn update_me(String);
        fn show_run_without_install();
        fn run_without_install();
        fn get_app_name();
        fn get_software_store_path();
        fn get_software_ext();
        fn open_url(String);
        fn change_id(String);
        fn get_async_job_status();
        fn post_request(String, String, String);
        fn is_ok_change_id();
        fn create_shortcut(String);
        fn discover();
        fn get_lan_peers();
        fn get_uuid();
        fn has_hwcodec();
        fn has_vram();
        fn get_langs();
        fn video_save_directory(bool);
        fn handle_relay_id(String);
        fn get_login_device_info();
        fn support_remove_wallpaper();
        fn has_valid_2fa();
        fn generate2fa();
        fn generate_2fa_img_src(String);
        fn verify2fa(String);
        fn check_hwcodec();
        fn verify_login(String, String);
        fn is_option_fixed(String);
        fn get_builtin_option(String);
        fn is_remote_modify_enabled_by_control_permissions();
    }
}

impl sciter::host::HostHandler for UIHostHandler {
    fn on_graphics_critical_failure(&mut self) {
        log::error!("Critical rendering error: e.g. DirectX gfx driver error. Most probably bad gfx drivers.");
    }
}

#[cfg(not(target_os = "linux"))]
fn get_sound_inputs() -> Vec<String> {
    let mut out = Vec::new();
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    if let Ok(devices) = host.devices() {
        for device in devices {
            if device.default_input_config().is_err() {
                continue;
            }
            if let Ok(name) = device.name() {
                out.push(name);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn get_sound_inputs() -> Vec<String> {
    crate::platform::linux::get_pa_sources()
        .drain(..)
        .map(|x| x.1)
        .collect()
}

// sacrifice some memory
pub fn value_crash_workaround(values: &[Value]) -> Arc<Vec<Value>> {
    let persist = Arc::new(values.to_vec());
    STUPID_VALUES.lock().unwrap().push(persist.clone());
    persist
}

pub fn get_icon() -> String {
    // 128x128
    #[cfg(target_os = "macos")]
    // 128x128 on 160x160 canvas, then shrink to 128, mac looks better with padding
    {
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAIAAAACACAYAAADDPmHLAAAjKUlEQVR42u2deZwc1XXvv/dW9T6j2TUarSAJLViWJStCWDYI2WFzwEtsYX9C/DH4EZGPEyzbWUhecPRI8pwXYzAQcB4xSbAxnydDRGyBkQ3BssCskpCMBdr3ERqNZp/ptarueX9U9agF0zMjedQzgj761EjT3aqqrnPuWX7n3HOgTGUqU5nKVKYylalMZSpTmcpUpjKVqUxlKtO7n9SoXVgpjDE6uAe1ZcsW9V568IsWLRKg/1BKybv+S4uIEhFrw4YN9mgK31ikDRs22CJii4gu5XXtEjFeb9myxVJKOYAHsGLFithtt902d9KkSfWhUOgDtm1XG2N4DwiGaK1xXbcrk8m83tLScuLll1/et3z58u5CYbjssstEKeWd09909erV+oEHHgjlf1+3bl19Z2fnZ3O53A9c190hIq6UyfU877Druj/u6elZ+frrr08veH72o48+ap2TzC9k/N69exel0+nvuK57dIAH4ImI8x49vHc8DM/rzWQyP2htbV2Wf36PPvpo+FzSjOqWW26JAGzdunVSKpX6F8/zcqd+R88NvrwpKwAxwbNw364Rs9nsY3v37p0HsHLlytCKFSvGtjZYsWKFtWLFijBAR0fHH7que6zg+zhlhg9bIPILRFzX7Ukmk7cH/poq1Kxjzd7bgAbsvr6++8uMHxn/IP+PTCbz7Nq1a5uCZz22TMLKlStDAGvXrm3KZDLPFqj6MuNHRiPkAiH4zZEjR5YA5M3sqNOyZcvs1atX208//fTETCazNbjpXJlvI06OiIjjON27du26pEATjB4SuHr1al1bWxtatWqVSafT/x2NRi8FHCBEmc4GuYDtOM6RH//4xx+6//77jwNs3LjRPdMT/jaokzp48GB41apV2Y6OjvvKzC8ZcOeFQqEp11577Togetlll+nRiA76Q70DBw7cUKiiylQ6c9DW1vavAF/84hejZ6rN9RmGe7q2tta74447pk2ZMuXbQUJDlxdoSTWBW1dX90ebN2++5vvf/35m9erVZ6R5z0R1qMWLF4fvueee3Jo1a+5OJBJLAXOG5yrTb6mJa2tr5zc3N/+/t956y1mxYoVs3LhRShHyWU8++eTScwHRMyLiGiOuEXGCwzVGPCOnHK4xBe/7v5tzBCfYvXv3VwtC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".into()
    }
    #[cfg(not(target_os = "macos"))] // 128x128 no padding
    {
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAIAAAACACAYAAADDPmHLAAAi5klEQVR42u2de5SdZX3vP7/nefd1Zs9kciUkgSSQi0EkiKCAFkS8LS3FtkoPtqvWdayK7bHW1eI59QBSuxRtBe2xiHKWrlaxxVYR9SgFFCoIcpEAmnsmIRcgIclkLvv+Ps/v/PG+786eSSazZzKTmQnzZM1aSWbvd7/7/X1/95twnEdVLYCIuKb/WwK8Cngd8FpgFrAK6AAUEGbOsU4IPAtUgEeAx4BHReS5pmcsgGl+7mM5chyEN4CKiMb/XgRcBbwVOB/omqHjuJ4+4GngLuDHIrKhiQ4yViDIGAg/CHmq+hbgD4ErgULzSwHf9Bkyw/mjOr7pOQpgmn7ngJ8BnxeR/zwaQ04IAFTViIiP//5K4HPA24eIruRmZ4g9vidhKAWCpv+/NwbCvYlKHo00kFEQPxCRUFULwF/HP+mmm5oh+okHQ7Nk+Ffg4yLyfEKrcQOAqqZEpK6qrwZuB85tEkV2hh6TelwTEJ4HPiYid7aqEsxI+j4WKXVVfT/wQEz8BF0zxJ/8Y2M6hsCpwL+p6s2xqpbYZhs9AJI3iohT1euB/xsbeW6IDpo5U+MEsVpwwF+o6ldjEJhjgUCOpfPji90GfCD++4yenx6nDqSAr4nIn8axGn80dWCOofND4PqY+LVY1MwQf3qcVKwSPhBLAgfYo0kCOVpkLxb77wO+HqMpmCH+tJYEt4nIh47mIsrR/HxVPRt4FMjOBHBOGhC8X0S+PhQEMjTCB7QBDwFnz7h5J1XMoAKcIyLbmgN6zTZAEt69NiZ+OEP8k+IkTN4G3BYbhDLol02ifxXwTJNvOSP6OakyjAHwxyLyz4kqGAqAe4C3nMyiX4+VjFYQOemTS9uBC4BDgJoYCV5V33gyEV8BpxB6JVRwqviYwMIwPwI+fm0Yv9dpdK2T4JgYBGfEBqEHAmlK8nwb+IMmUTH9LB0FVcWIYGR4NuivKYKiGpFWRFCEQlqGDY16Ba+KxNeW6SsFBNgEvBqoJirgFGArkD/eQpHJIDqAHXLHPVXHpp46z/XV2VOG9XsH6HWG/cUae6uKuvCwvFdFbMCCjDC3LU2n9axZ0M6iHJzekWJVV4quzGCh6DSxnKcdGHwsDd4hIv8vAcDHgb+fLtzvY7HcTPSBuvL0/ho/3TnAugMhGw+U6dU0YgOwAbgw4ngfomH9SGWvigQpMAEicvg9LqRTaqyek2PtnIDLTmvnnLlp2lMyCAwSg4HpkT00wN3AuxIA3AtcPtX1/1DC99c8j+yt8uOdJX6+p8yLVYvYFOpDtF7F+DqisYiPxb0KqB/GXzKQRMuT96iANykklUFMgLo6p2Qcb1iU4+2n5blwQYZC2kwnICRm8AFghcQFnOuA2VO1YHMo4bf21rl7R4U7NvXxYs0ixqLVMuJrGDGoV7wqOk4mvcR2hRjBq0dNGsnkUO84Je24elUHVyzNcmZnaroAIbFr3y2qegXw/alI/ETHJ4Tf1FPjK+v7uau7RBjk0EoJcVWMsfjYyj8h5rSAEcF7h9oMks0ThGWuXJ7nQ2sKrOpKN4AwRW2ERNJ/SVT1BqKs35QS/04VG3Pwxp4at63v53tbS7hUDl/qxarHI+PG5cclHVCcGEy+E1sv864z83xwTYHVDSAc/i5TzBB8SJr0vx+pQuhEBWp8LO77ao7bflPky8/04tI5fLEXi8djppxvHiVSPA6DaevE1sp85FWdfPCsNjrSltAr1shUkwYDoqpPAOdNBQB4Paw3H9hd4rrHDrG9lsUP9EQcL2bKB2UEMOojidDexbJ0hRsv6OLSxbmGWrBTCAWiqr1MgY6d5MGEXvn0Ywe5fUsNvMOG1WkZjZNYirkgA2L4k5U5PnpOgbm5YBDQJ1vgiib+0aTq++hhbe+t8bGHDvFkfwDFg6Dgp3k+yiQJhnwn82yNL1zYyaVL2qaMXSCq6ieT80OvBEZ4YHeRa/7rIH2axlYGCHViDbejs8PEPYZAwJkAMnm+8ros71xemBLqYFIlQKjRg/nWxl6ufaICtTLGh7hxwqMAIooRA6p4Il3ijY2iPoOo7zHegYBBQGKfX2Xc1I9F0VSaQsryw3fMZ1lnCp1kdWAmn/h9XPtEBVMrIeNAfFHFEv0ooEEWny3g87PQdBuabScjno7AUzCOgnF0BJ6MeDTbjqbb8PlZ+GwBDbJRACq+nhwnrzgEU6/RH7TxLxt7kaYc7ctKAiTEv+nxA/zjZoepDuCdQ8UcR3BGMQihGEymDUQwtRLLOtOcOzdgaSFg7bwMKWtZWrC0Bc2ljkoxVHb0O+rOse6lKjv6Q57aH7K9t4ZP5yMJUi0SqMejeJUx36cGWZbmHfe9awkZaya1BiGYDIMv4fzDxPdjJr4RRRR8kMOnsxTCIhctNLzt9Dxr53axvCMgaOEBd2bg1LbocVyyKN8AandfyLr9NX7yXIlf7LEUgw60VsHWy1GeYJRAUA/qHfvLSsUp2WBy3a8TKgESgy/R+aZWijlfxhifB5/KIekMK7M13r2ind9Z3sbC/JGpWx3Upx7l9YdURKPxK7TJjWs+L5Qc3+8u8p0tA2yupNFaFVMvR7mKFr+DqKJBioJ1/PKq0+nM2JcHABKL9+d7Svy3B/ox1bET36rHB2kkk2dlW8gH1hT4vTPaSMXWlAe8jxM4jL3MSzXJRyjGHC4WqXvlP7YV+dr6fjYXA7RawoQ1XAtSzAJk85zd7rj7dxZPetLohAAg6lKEXQN13vHDfRyqhVCvj9rHF1VEFPJdzAtqXPvqTn53WZ5UzKqjDbf6OC8syBESYdgys1iKAdSd8t3tJW76VS8vhWko9URewzGuFYjiO+bzv1Y5PvyqroY9dNICIMnoCXDFD/awrj/AlPpa4pahLpQzFsnk+e1FlhvOn8WCWGc7jQIu0iKra+zYNUNF1SMt3pOq4pGGithbDLnh8UP8YLdDq0Ws+qN6M4GAyxVYla3z3XcupJAyjRrFkxYAiei/7pH9fP05gyn2jDrIY9Xjs+20B8p153Vw9cr2mOPBSuuEH3q2HFpPxZVZ07UWayLHcTTpGlXFqRDEuPnmpn7+5rE+nApSK2Ga7iv0isl3MDfl+fblXayenZkS2bcJBUBC/IeeL/EH9/dhwzKh86OOoPlsgbM6hM9fWODsudkx59kTzu+v9nLL0zey7uCjiBEW55fyZ2d9khVdrxg1CIbWLTy+t8InftnHpp4aks41XiHVEpcsyvH3F81iQVtqyuQDJgwAGhtRpdDzlrueZ1cJqFdGpfcDUXyuk1d1OO64fC6d2eC4dKZXj4jwxSf/lkfL91E7EIIYbDvM1VO4+dJvkrXZMYGgOe/vVXlqf51fvFAhVKEjLfzWqVlWdAZHZD2ZAkMFJiy1awX+z7oedvk2THiAcKzEf/M8OjO2EUMYGyCjkHCxNsAv9z5IVcPY9QvRPmFf/nm2HtrAK+eci+rY1EpE/Kha6Lx5ac6blz5qHdZUKhULJqzcRGBbb42vrC8jro4bRcDEqsflOjmnI+SON8+nM2MbRSLHL5kUYwzaZIh4AatRiRfjUC4WFbUcjiskMfepWCNoJqqqR4AvPdWDS2ej8uqWbygy+Fa1Dya+GW/xdBRgyHgVkUokDQKh8TNVC0SDiSk2U7b11rl7Vx3qRXyL7pWoIqk0OaN8+Q1zGmK/Fc6PxLmOyq8f5YzECb3+SSMBNK7D/9JTPYRBFnGude6PQ7t/+5o2Vs/OELZMfB+3bBmMmLim34/jd5rY6580EiAx/Hb217l7Zw2t11tO8lj1aPts3rmgxlWrOlu29p13kQ+vypbe3wCwovMsRMyYrfkjVYM56vWTz54BwBAr984t/YSpPKZ6sKWInxHQVJa5VPj0hadElnSLbp01lu5Dm7lt/ecbBFo1+5V8+BWf4LSO5WO26BOxLyLs7Ovm1g2fZdPBXzcA8ME1f8XyWSvx6qOCk5e7Ckg6d4p1z7+s70OrxZZ9flEP6Rz/87x25uZsw4s4ts/tMGJ4aPd9XP/En7GtuB5XU6jBtvIGbvnVDYS+zvFnMOvc8qsb2FbeADVwNWVbcT3XP/FnPLT7PowYnLoZACSG9aMvljlADgmrLWX6DIqm8yy1Ja48ozCoE+hYxLdi+cWen/HFTTfQX+4nLHlAcXjqvY4dlS10925G4tKusQaNuns3s6OyhXqvw8VjkcOSp7/czxc33cAv9vwMK3bagmDcZdcPugcQY1sWiwKQyvI/zp1F2poRc+OKYsWytWcDt274DK4a5Wy1ydT0sUtXdZXj/j5VV4lA1HR9jWsLXVW5dcNn2NqzASt20D28rACQiP/+mueB3WV8rYRroVHPoPh0juVBid9ZXhgxRJo84P5aH59/6pMU3QCu5mmOZls12KxQ0C6Wd64as38fDY1QlneuoqBd2Kxg1QxOBNU8RTfA55/6JP21vkH3+LICQELrXx+ost9nMK7WmvgXQdI53vuKAmkrjFSfrhpZ9bc+/VkO6l60zOF+7iS9m4V8qp0PnfPXtKXaG+8ZfUVx1B7eFl8rn2qH7OAUMqJoGQ7qXm59+rON97xsVcC9O4tIkIrKqlsI+oRiydf7uXJ5RKhj6f7E2n54z095rPdB3IDHix9EMJs3nFFYzU3n387rFl5y3Ba6EYNXz+sWXsJN59/OGYXV2LwZBAIvHjfgeaz3QR7e89PGe15WAEgex6/2VtBkEkcLrp9k8rx2fpr5+QCV4fk04eLQh3xzwz+hdRgcXhJMVliSXsr153+RRYXTxs09Swi6qHAa15//RZakl2KyMkhWORStwzc3/BOhD6eVJDDjof+NwMGKY1u/Q+vVlvv0RQzvXN4+XHi+uYENEeHB3ffwEi/gar4RdRAVbCDkbI6Prb2R9nSh4SKO3zyAyNVrTxf42NobydkcNhBED5eVu5rnJV7gwd33RPaDvEwAkBBue1+dHpfC+HpL+t+LwdbLvGZ+dsQbSTjqR93/jrohlSACJiNcvfzDnN55RsNFHO+TuHqnd57B1cs/jMkceR/qJLpHPf4I5PSRAJqUV9WQIEUrwI+aIzIsznqWtAfHbI/y6hGELYfW81xlM2HVHR7vhmDSwjyzmLcte9eER+USdfC2Ze9inlmMSUuD0KpKWHU8V9nMlkPrEWRa2AJmvAyAXf31wxO2RvxQwaQyXLAwRyq2/kdy/R7ecz8mJdjmugIBkxLes+J9EdcLE8p5En0AVizvWfE+TGqwFLAqmJTw8J77p41LaMbrAhsO1FBXb934MYbl8VAlHUn0esez+5/A1X3D+BMVsNDuZ3HhwjeOS+KnZfcQ5cKFb6TdzwJLwxZwKK7uo3v1E6OKpqwb2F/Xw/pgxJoBRetVzp6bOeZUyubAzwvl5/F1bagYI0KQtayd81qyQfaEAyAbZFk757UEWduo/hUFX1deKD8/bQJD5rgHzglUQs9LZRcNYRxF2UwwQsYnkSYvlnZTM+Wory5GQNTqraydf/6g13JC5hhFn7V2/vmH285jb0U91EyZF0u7T/h9MRnpYAEqTnl+IERVRux0FFXUWNq0xtJC0NJc2p7qgWiIoxwWMgpoXTmlbfERTR4nQgoAnNK2GK0P5nGRaPRAT/XAyysQFDVHtNiZIwarjs60bTy0Y6mA7kObESsYlYb+FwsZ8ixsW9KI3Z8wAMSftbBtCRnySJMdYFQQK3Qf2nzyq4DBRB3968MWI0bDVd2IQmAmb7RxYIJh3d7pUik0fUtZZs7UAsBoI58R97YmNtww9fqRFAnHVrd9FL0+Wjsi9OGwks959/KqBwj9SB794BIwJ5bemhsUTRzO2Fo+ayXqFC9N1raDKiVeKO4albWtKN77wbeq0TVDDUflBbxQ3EWVEuqavBNR1CnLZ60ctXHqVRsbTqItJxM/H3FckkFZK5zaHiA2NeIwBhVB1FGUNDv6Q1qBTVdmTuQC6pAJYCnhxeLuURlbGZtlVqYLmzIYG2AxWGvQEOZmF7REtOSzXizuRlKDX60ajYHpyswZ3XjcuKUsMIcbSmwcaHQT6Eqa47X+VSEbGOblbBQKHsW3HskITKztU/KLSftcPM9fGuFkRFi37/GWvIAkNp+2af5ozUeQACTrkSykZlneseQqFhdOb9QCtnJf6/Y9Hg2josk7MZD2OU7JL27pvpJYihF4seS4c2uJL6zr4wvrevnPXWUGaq7RcDqly8ILKWl5FotB8KkMz+6vcsmi/LC8m/BWId3Bwtyp7KhsbZSaeVV8xbHuwC+phBUyQWbEaGCSKLro1DfSFtzCj3f9O6V6iUsXvY3LlryjkXgasU8AoRJWWHfgl4R114h9qIBNCQuzp1JId4woTZKQSd15/u7xXr69vUzJB2Cj1X5aP8SCnOHjr8pz9cqJGSwZjNfc8VfMSXPvwahrpiVp7D3dvX7EQJDTqPni7LmvYdfebqhG+QAVRZwwkDnEIy/8jDcueTvRAHlpqeL3nPnnc04cRRwKkBGbSzE88sLPGDCHoEn/WwSbMpw99zVYY4+Zmk4eUSn0XH3PXp7sz+BLFWyTBFJV9oZprn0y4NmXXuIzF88b9z5JM17dIEsKqSgU3IKo8ii+XuWxF8rUnY5YCwBw8aI34euKa3Y34tj7nVu+EZVla2u2QJLW9Rr18Laatm1UA6vjzi3fwNcHW2lOFF9XLl70ppa4X1X5m4f382QxjykeRHw0WqZhBCLRFpRyL9/cafjXzX2YpoVVUwIAidRfMSuNhvWWAkJeBQmr7K4Ydg2EjX19wxFLUVbMWsPp2ZUEGXuYQ1B8TXnJ7+Yn2783qnq8Rp8f0nINQVJv8JPt3+Mlvxtfa2oYFSHIWE7PrmTFrDWNeQTDTU4xwDP7a3xnN5iBA4TDDJfyKpHXUi9x46MHKNY9hvHzDsx49MMDLOtI0WXr0YKlVmoC1eNSOZ7YVxm01nL4/jzhHct/H7F6hAvnq8od3bfyXO82rNgJKcTw6rFiea53G3d034qvHnkfYjW6x7ikfKQimru29TfmGB9bYgpSq9IvOR56vnxMhjnxEiAuC5udtZxRsEgq03IvvKrnh90DI07NEI1Kwi5Z/FbmsRCbPrzWWEVxoVJ2ZW5edx0Dteih+nGcwuuJOH+g1s/N666j7Mq4UJvq/gSbNsxjIZcsfmtUEqYyItM89VIF36LaFBFIpXlmfxWmkgRovplXL8gitrWqIK+g1RK/3FdjXymMsoQjNGkEJuAPX3ENkooMruY78BVlV20Hn3r8owzU+jCMT8+eU4fBMFDr41OPf5RdtR34ig4igUWQFPzhK64hMMExm1ESy39fKWRzn0NrlVFxs07lXMCbT2tDw/qgNqpjBYQCdZRSBe7qHkBEjmncJPr94kWXcUHnJdh2g2nu1EFxJU93aRPXP/HnbDu0qWGBJ8beaFrCEzVixbLtUHTN7tImXGnwtYwabLvhgs5LuHjRZSPWJbp4f8Rd3QMM2AKBtjYtNVp66VhSCKYeABKR9so5GeaaKt6mW7IDvCpaK/OtDf3UYm9AW2jX+vA5n2C2LEByg9OQCQh29G/huseu4Uc7vkPo6w1jz6vHedcARPOf5t8lhmHo6/xox3e47rFr2NG/5Qjio4LkYLYs4MPnfGLENjSNH3jNKd/a0I/Wyi0FeEQVZwJyYYk3L2k76qrcSa8HcAqFtOHSxTlMOo9twRDwCKZWpjvM8/3u/mhCiI7sEhbSHfzVuZ+mzbZj02bQQ1eUsKIUqyW+vvUW/vLh93H/rh9SCosYMdi4cVWG/Gn+XSkscv+uH/KXD7+Pr2+9hWK1RFjRQcQXifR+m23nr879dEuBn6T38fvd/XSHeUyt3FILvTWCZNt4y5Isc3O2sZhySs0JTKJU9+8q8scPDiCVPnwL+DIopHMszTru/73FBGbk8anN7eE3b/zf1EsO/OAYgKg0egaMFTqZy9r557Nm9loW5BexpH1ZQ0U4dewa2M7e0h7WH1zHun2P08t+vNOGta9DexANpPKWj63+Wy5a9MYR+xGSuYmh97zpP3azo2KhRQAYPJrt4J8vaeeyJW3jGhEcNwAkxk2x7rno33ZyIDRQb61J1OLRti7+4bwU71nZ2niY5IE/tPs+btv4OUquiC/roH7BRuBZFLFgszaq4QuVjM834OmBqilhgiic7SouyvDpkTs9jBpMTsjbNj64+q95/eLLW2pGSb7TnZt7+fiTdaTYg2uBQUSjNTNzA8/D71lCWyqqjZMpVw8QS4G2lOGP1nQgmbaW1zuqGKiV+cyTA+wvR49lJMvYSrRC8vWLL+e6825hQW4xtpA0bsrguKMq6gQ/4AlLHleDsi9SjH/KvoirQVjy+AGPuqS3zx9RMWALhgW5xVx33i28fvHlePyIxE9G3uwvOz7z5ADUyi3PTjIoJtPGH63poD1tWwx2T5IXkNzYe1YUCOolvLEt3axXkFqF/WT55CP7IlugpZuPXL0VXWv47Gu/yoVzLiPIG2yjY2fwNHAXgwH1EDL4R6PfRa85CuHTQpA3XDjnMj772q+yomtNw0VsdXDmJx/Zx36ySIuunxAtuArqJd6zogATsFhi3GcFJ/rpow/s5bsvWGSgp+XR8BbF5zr4h9dkRjUprDkL+PM99/Lv27/BnsoO1IEre4wIjqinYKSmTVGJsnpEKVibM4iFRdml/P6y9/GGRW8+4jNbEf3/tqmXjz9RxZT7Wl6MFU1O6+J3Fzq+eOmCCckGjjsAfKy3uvvqXH73PsJ6DfWteeGiikmlyKZS3P32OayenWl9UGRkZiEYQlfnwefv4b7dd7Ox51mCjMWFPorda9TaYYZE6rzEJBXFpAUbGMKqY3XX2Vy++AouOfWtBDaFxmMsWiF+cu8bD1a54scHqNTr+HprzbMCiBGCVJr7rpjP8o4UKjLuRZwTMi38eKSAwaOZdlZ1KN9924JRj4odGojZ2PMsj774AL858BS7+3dSpYQEkWcw6H1O0VDJkGdx4TTOmnMurzvlUlZ3nT3stVtJk/dWHb/7k71s6hOkOtCSZ3SiuH/CAJCMeunurfGmu/bhXR3vfcuxOKsen+/knE43pnnByVhXkcGc2l/vZWdfN/31Pp7r34rGiliMcHrhTAqpDk7rWE4h1TlIsiTt3q32HjQT/+p79/F0r8WUeltmAgGMMRib4v4r57O8M90IIk2bfQEJYj/7+AG+3G0aKU+Oc1z8aLkgifoZTOsrZVQbxSWjbTdP7jEi/ks802cx5d7Rf/f2OXxkuecT58+Z0BWzL6uFEQk3H62INJEUQ6UGY1iI2VsJufq+/WMivkEhlWVJHv7zylPJBxO7V8hM5Pp0BdpThs9dPBs1AcbaUT5QwZT7eKbXcNW9PTy7vxItXhpjuXTC0UYMVuygn+YCkbGAPVlm8ez+Clfd28MzvQZT7hsV8QGMtagJ+NzFs2lPmQnfKTihnUE2JtbrT83z/jMDfK5z1NwbKkipl18fCnn3vQe5Y/NAo1w69JPbfauqhP7wksk7Ng/w7nsP8utDIVLqHfVyrEDA5zp5/5kBrz81f0K2i78s18aNB+GPujZuj0OrJax3o16CHRm+HawthNz924saw7dkZnHkxC2OHJOoP87FkcPr/RSz0gE/eud8lrSnUDkxjZszq2NP8OrYowa/rMVn8nz70gJvWHRiRP/M8uhJWh59VOKn89z0mizvXd3ZeEacjNvDmyXBtzb2ce2Tlab18XJ86+NTOSSdpS0sctGifLw+Pt3y+vjhDNDB6+NLFIM2tFbBjHF9/GDiG3ymnZvOy/Le1R0nlPMnDQDN/vJNjx/gHze7GASu5RTpcEAwCKEYTKYt6tmrlVjWmebcuQFLCwFr52VIWcvSgqUtaM4WKsVQ2dHvqDvHupeq7OgPeWp/yPbeGj6dj+oIqkUC9VFji8pxPHQfi/12/nyl5drz50zaEulJAUAzCL61sY9rnyhjaiXUu1EbhsOphmhsmyCpDJLKgBg0rIEI6XqRTGAGhYKroaeWagNVJEhH6eF6Fa1XsejhSubj9DQMiphE7Od47+qOSd0gPmkAGAyCyCagVsb4cNQu1DEzaqKN5gsft3Z5Y6NJToOMAI/xLiojizuPvfrIqh+vuAiKNwGkc4d1/sm+Pr5Vw/CB3UWu+a+D9GkaWxkYdRBltFJiuFL1iTqBgMu20yE1/um3ZnPp4rYTbvBNSQA0G4bbe2t87KFDPNkfQPFgxK3TZOjyMX18Adpmc14h5ObXz2JZZ3pSDL7hAFCfyCXSowVB6JVPP3aQ27fUwDtsWD0ho1ImIhdiBVyQAWP57yvSfPKC2QRGpgzxEwA8BaxtSmNP2mneGfTA7hLXPXaI7bUsfqAnCv6ImfJAkKTxVQymvYtl6Qo3XjCLSxfnp9zq+AQADwMXTQUANOblxNzTV3Pc9psiX36mF5fO4Yu9RLXAUw8IElczOQymrRNbK/ORV3XywbPa6EjbRkv4FFs5HIqq3gz8BeCAKTPd0Kli46e1safGbev7+d7WEi6Vw5d6I4mATKjh1rLbiUYcn+/E1su868w8H1xTYHVX+ojvMkVOwuzPiKr+AfDtqSIBjpZJTPTlpp4aX1nfz13dJcIgh1ZKiKtijI1mBp0gsWAkmujlvUNtBsnmCcIyVy7P86E1BVY1CH9iMnpj4a+Y2W8XVV0CrAfaYcLrD8ZsG2hTbH5rb527d1S4Y1MfL9YsYixaLUfjVCQK8HjVcZMOUXBJEBPHBkwayeRQ7zgl7bh6VQdXLM1yZrz/IOndM1OQ8kMkwFWiqhZ4ELh4qqmBkYDQX/M8srfKj3eW+PmeMi9WLWJTqA/RehXj60i8zr5RCiZRX8hRCW0OTzxN3qNCNPUklUFMgLo6p2Qcb1iU4+2n5blwQYZC2kwXwjfnug4Br5Q4E/aXwD/EPTIBU/wMBQLAQF15en+Nn+4cYN2BkI0HyvRqGrEB2ACSdXY+RMP6kdaYKhKkDq+9Sd7jQjqlxuo5OdbOCbjstHbOmZumPSWDe/6nPuGHiv8fiMgVCQBWxmrAwPSJvGhTD+FQv7qn6tjUU+e5vjp7yrB+7wC9zrC/WGNvVVEXHgaBKmIDFmSEuW1pOq1nzYJ2FuXg9I4Uq7pSdGWOTDHDlNXxrQDgahH5dqICPPBT4NKprgZGAoPG+nrYLWRAf02jxS+qTYMnhEJ6+M4br1FhiMTXnqbxycRM3g8sA0pB/ABUVW+MATAtv1ujgEPkCEBE7dQaVwVDZ1qG/Zo+njWoROtJEoInlv80Py5W8beKSFFVg0QFGBHxqvoz4JKYUSwn4VE95pYqpj+NR+T+HmAVcKC5LDz52p8CTt5HkOz0YZifk/qb4+Kv+QUR2R8tXmsaZqeqVkScqt4JvHu62gIz55h+/2bg7GQiwlAAJEO65gG/BuY2DbaaOdOf+w3wFhG5L2H2QZ1BIuJjsbAPuCaZ+sL0y8TOnMGnHkvyG2PiBwnxjzpxJH5B2JQkqgOpmec4bYmfAu4TkTc3c/6xABB5VBEIvgp8YLpECGfOoJPQ7HHg7cDBxOXnWM2h8Qtc7Br+KfC1+EL1mWc6rTg/If5bReRARNojByTJMRogJbYJ3BBJYE92V/EkEfsJ8XuSOA+jaQ+P0eJjvfGnwG0xqhLjcOZMPVfPx8S/pxXijzgfYAgIPhRLgd5YCoSAn3nuTIUIXxjT0gCfEpG3tUL8Uc0dbAoXrwS+BLy1ydgwM/GCSSG8azLOfwN8XETuSWI6R9P5YwZAc7Qw/vv7gWuBlU3BBqZbSnk6Jj1JGo2isw/4R+BzIlJL3HhGOd11NCBooEtVZ8Vq4Y+Bs45yozIjGcZFt+tRnuUe4HaizN7eoQw6YQAYRhqkY5XwJ0Ql5gtm6DYh5yBR+d53ge+LSH9CC8C3IvLHDQBDg0ZN/zeHqK7gIuBC4JVAYaoWnE6DYM6zsUv3U+AREdnZHLUF3FgIn5z/Dzgq7v6ou8bpAAAAAElFTkSuQmCC".into()
    }
}
