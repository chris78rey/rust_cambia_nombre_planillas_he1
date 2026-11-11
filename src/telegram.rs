use reqwest::blocking::{Client, multipart};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

const POLL_TIMEOUT_SECONDS: u64 = 30;
const RETRY_DELAY_SECONDS: u64 = 5;
const BACKUP_CLEANUP_INTERVAL_SECONDS: u64 = 60 * 60;
const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 12;

#[derive(Debug, Clone)]
struct TelegramConfig {
    token: String,
    chat_id: i64,
    default_input: PathBuf,
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    error_code: Option<i64>,
}

struct TelegramHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

struct BackupCleanupWorker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

struct TelegramProgressTracker {
    current_folder: Arc<AtomicUsize>,
    total_folders: Arc<AtomicUsize>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    chat: Chat,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Chat {
    id: i64,
}

enum TelegramCommand {
    Help,
    Exit,
    ShowAdvancedMenu,
    BeginProcess,
    BeginCleanupAuxTemporal,
    BeginProcessReport,
    BeginFillPaths,
    BeginRestore,
    BeginReport,
    Process {
        label: String,
        input: Option<PathBuf>,
    },
    ProcessReport {
        label: String,
        input: Option<PathBuf>,
    },
    Restore {
        target: String,
    },
    Report {
        target: String,
    },
    FillPaths {
        field: String,
        value: Option<String>,
    },
    CleanupAuxTemporal {
        input: Option<PathBuf>,
    },
    Check {
        target: String,
        folder: PathBuf,
    },
    Unknown(String),
}

#[derive(Debug, Clone)]
enum PendingAction {
    FillPathsField,
    FillPathsValue { field: String },
    CleanupAuxTemporalConfirm { targets: Vec<PathBuf> },
    Restore,
    Report,
}

#[derive(Debug, Default)]
struct CleanupResult {
    deleted: usize,
    missing: usize,
    errors: usize,
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let config = TelegramConfig::from_env()?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mut offset: i64 = 0;
    let mut consecutive_poll_errors: u32 = 0;
    let _cleanup_worker = BackupCleanupWorker::start();

    eprintln!(
        "telegram activo: chat_id={}, input por defecto={}",
        config.chat_id,
        config.default_input.display()
    );
    let _ = send_message(
        &client,
        &config,
        &format!(
            "Bot de Telegram iniciado correctamente.\nEntrada por defecto: {}",
            config.default_input.display()
        ),
    );

    loop {
        match get_updates(&client, &config, offset) {
            Ok(updates) => {
                consecutive_poll_errors = 0;
                for update in updates {
                    offset = update.update_id + 1;
                    if let Some(message) = update.message {
                        if message.chat.id != config.chat_id {
                            continue;
                        }
                        if let Err(err) = handle_message(&client, &config, &message) {
                            let _ = send_message(
                                &client,
                                &config,
                                &format!("Error al procesar el comando: {}", err),
                            );
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("telegram polling error: {}", err);
                consecutive_poll_errors = consecutive_poll_errors.saturating_add(1);
                if consecutive_poll_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                    let _ = send_message(
                        &client,
                        &config,
                        &format!(
                            "Telegram polling detuvo la respuesta tras {} errores consecutivos.\nUltimo error: {}\nEl proceso se detendra para que systemd lo reinicie.",
                            MAX_CONSECUTIVE_POLL_ERRORS,
                            err
                        ),
                    );
                    return Err(format!(
                        "demasiados errores consecutivos de polling: {}",
                        err
                    )
                    .into());
                }
                thread::sleep(Duration::from_secs(RETRY_DELAY_SECONDS));
            }
        }
    }
}

pub fn send_operator_notice_from_env(message: &str) -> Result<(), Box<dyn Error>> {
    let config = TelegramConfig::from_env()?;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    send_message(&client, &config, message)
}

impl TelegramConfig {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        let token =
            env::var("TELEGRAM_BOT_TOKEN").map_err(|_| "falta TELEGRAM_BOT_TOKEN en el entorno")?;
        let chat_id = env::var("TELEGRAM_CHAT_ID")
            .map_err(|_| "falta TELEGRAM_CHAT_ID en el entorno")?
            .parse::<i64>()
            .map_err(|_| "TELEGRAM_CHAT_ID debe ser un numero entero")?;
        let default_input =
            PathBuf::from(env::var("HE1_INPUT").map_err(|_| "falta HE1_INPUT en el entorno")?);

        Ok(Self {
            token,
            chat_id,
            default_input,
        })
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }
}

impl TelegramHeartbeat {
    fn start(
        client: Client,
        config: TelegramConfig,
        label: String,
        input: PathBuf,
        current_folder: Arc<AtomicUsize>,
        total_folders: Arc<AtomicUsize>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let start = Instant::now();
            loop {
                thread::sleep(Duration::from_secs(30));
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }

                let elapsed_seconds = start.elapsed().as_secs() as i64;
                let elapsed = super::format_duration_seconds(elapsed_seconds);
                let current = current_folder.load(Ordering::SeqCst);
                let total = total_folders.load(Ordering::SeqCst);
                let folder_progress = if total > 0 {
                    format!("Carpetas: {}/{}", current, total)
                } else {
                    "Carpetas: no registradas".to_string()
                };
                let message = format!(
                    "Sigue procesando {label}\n\
                     Entrada: {}\n\
                     {}\n\
                     Transcurrido: {}",
                    input.display(),
                    folder_progress,
                    elapsed
                );

                if let Err(err) = send_message(&client, &config, &message) {
                    eprintln!("telegram heartbeat error: {}", err);
                }
            }
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for TelegramHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl BackupCleanupWorker {
    fn start() -> Self {
        if let Err(err) = super::cleanup_expired_backups() {
            eprintln!("telegram retention error inicial: {}", err);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(BACKUP_CLEANUP_INTERVAL_SECONDS));
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }

                if let Err(err) = super::cleanup_expired_backups() {
                    eprintln!("telegram retention error: {}", err);
                }
            }
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for BackupCleanupWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl TelegramProgressTracker {
    fn new() -> Self {
        Self {
            current_folder: Arc::new(AtomicUsize::new(0)),
            total_folders: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn current_folder(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.current_folder)
    }

    fn total_folders(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.total_folders)
    }
}

impl super::ProgressSink for TelegramProgressTracker {
    fn note(&self, _message: &str) {}

    fn update_folder_progress(&self, current: usize, total: usize) {
        self.current_folder.store(current, Ordering::SeqCst);
        self.total_folders.store(total, Ordering::SeqCst);
    }
}

fn handle_message(
    client: &Client,
    config: &TelegramConfig,
    message: &Message,
) -> Result<(), Box<dyn Error>> {
    let Some(text) = message.text.as_deref() else {
        return Ok(());
    };
    let input_text = text.trim();

    if matches!(normalize_command(input_text), "/start" | "/help" | "/ayuda") {
        clear_pending_action(message.chat.id);
        send_main_menu_message(
            client,
            config,
            "Flujo principal: 1. Preparar directorios -> 2. Procesar PDFs. La limpieza de .he1_aux_temporal ocurre al terminar el paso 2.",
        )?;
        return Ok(());
    }

    if is_cancel_text(input_text) {
        clear_pending_action(message.chat.id);
        send_message_with_markup(
            client,
            config,
            "Operacion cancelada. Escribe /help para mostrar el menu otra vez.",
            json!({
                "remove_keyboard": true,
                "selective": false
            }),
        )?;
        return Ok(());
    }

    if let Some(action) = take_pending_action(message.chat.id) {
        return match action {
            PendingAction::FillPathsField => {
                let field = input_text;
                if field.is_empty() {
                    send_fill_paths_field_menu(
                        client,
                        config,
                        "El campo no puede estar vacio. Elige uno de los botones o escribelo.",
                    )?;
                    set_pending_action(message.chat.id, PendingAction::FillPathsField);
                    return Ok(());
                }

                if let Err(err) = validate_ident(field) {
                    send_fill_paths_field_menu(
                        client,
                        config,
                        &format!("Campo invalido: {}. Elige otro campo valido.", err),
                    )?;
                    set_pending_action(message.chat.id, PendingAction::FillPathsField);
                    return Ok(());
                }

                set_pending_action(
                    message.chat.id,
                    PendingAction::FillPathsValue {
                        field: field.to_string(),
                    },
                );
                send_message(
                    client,
                    config,
                    &format!(
                        "Campo seleccionado: {}\nAhora escribe el valor de filtro, por ejemplo 16364.",
                        field
                    ),
                )
            }
            PendingAction::FillPathsValue { field } => {
                let value = input_text;
                if value.is_empty() {
                    set_pending_action(
                        message.chat.id,
                        PendingAction::FillPathsValue {
                            field: field.clone(),
                        },
                    );
                    send_message(
                        client,
                        config,
                        "El valor no puede estar vacio. Escribe un valor de filtro valido.",
                    )?;
                    return Ok(());
                }

                fill_paths_from_telegram(client, config, &field, value)
            }
            PendingAction::CleanupAuxTemporalConfirm { targets } => {
                let normalized = normalize_command(input_text).to_ascii_uppercase();
                let confirmed = matches!(
                    normalized.as_str(),
                    "BORRAR" | "/BORRAR" | "SI" | "S" | "CONFIRMAR" | "OK"
                ) || matches!(input_text, "BORRAR" | "borrar" | "Borrar" | "si" | "Si" | "SI");

                if !confirmed {
                    set_pending_action(
                        message.chat.id,
                        PendingAction::CleanupAuxTemporalConfirm { targets },
                    );
                    send_message(
                        client,
                        config,
                        "Escribe BORRAR para confirmar la limpieza o Salir para cancelar.",
                    )?;
                    return Ok(());
                }

                let mut deleted = 0usize;
                let mut missing = 0usize;
                let mut errors = 0usize;

                for target in targets {
                    if !target.exists() {
                        missing += 1;
                        continue;
                    }

                    match fs::remove_dir_all(&target) {
                        Ok(_) => deleted += 1,
                        Err(err) => {
                            errors += 1;
                            eprintln!("error al borrar {}: {}", target.display(), err);
                        }
                    }
                }

                send_main_menu_message(
                    client,
                    config,
                    &format!(
                        "Limpieza manual terminada.\nBorradas: {}\nNo encontradas: {}\nErrores: {}.",
                        deleted, missing, errors
                    ),
                )
            }
            PendingAction::Restore => {
                let label = input_text;
                if label.is_empty() {
                    send_main_menu_message(
                        client,
                        config,
                        "La etiqueta no puede estar vacia. Escribe una etiqueta valida.",
                    )?;
                    set_pending_action(message.chat.id, PendingAction::Restore);
                    return Ok(());
                }
                send_message(client, config, &format!("Restaurando {} ...", label))?;
                super::restore_from_backup(label)?;
                let manifest_path = super::manifest_path_from_target(label)?;
                send_message(
                    client,
                    config,
                    &format!(
                        "Restauracion completada.\nManifest: {}",
                        manifest_path.display()
                    ),
                )
            }
            PendingAction::Report => {
                let label = input_text;
                if label.is_empty() {
                    send_main_menu_message(
                        client,
                        config,
                        "La etiqueta no puede estar vacia. Escribe una etiqueta valida.",
                    )?;
                    set_pending_action(message.chat.id, PendingAction::Report);
                    return Ok(());
                }
                send_message(
                    client,
                    config,
                    &format!("Generando reporte para {} ...", label),
                )?;
                super::generate_html_report(label)?;
                let report_path = super::report_path_from_target(label)?;
                send_document(client, config, &report_path, "Reporte HTML generado")
            }
        };
    }

    let command = parse_command(text)?;
    match command {
        TelegramCommand::Help => {
            send_main_menu_message(
                client,
                config,
                "Flujo principal: 1. Preparar directorios -> 2. Procesar PDFs. La limpieza de .he1_aux_temporal ocurre al terminar el paso 2.",
            )?;
        }
        TelegramCommand::Exit => {
            clear_pending_action(message.chat.id);
            send_message_with_markup(
                client,
                config,
                "Menú oculto. Escribe /help para mostrar los botones otra vez.",
                json!({
                    "remove_keyboard": true,
                    "selective": false
                }),
            )?;
        }
        TelegramCommand::ShowAdvancedMenu => {
            send_advanced_menu_message(
                client,
                config,
                "Menu avanzado. Usa estos comandos solo cuando salgas del flujo principal.",
            )?;
        }
        TelegramCommand::BeginProcess => {
            let label = super::auto_process_label();
            send_message(
                client,
                config,
                &format!(
                    "Usare la etiqueta automatica: {}\nProcesando con la entrada por defecto.",
                    label
                ),
            )?;
            let input_path = &config.default_input;
            process_and_reply(client, config, &label, input_path, false)?;
        }
        TelegramCommand::BeginCleanupAuxTemporal => {
            begin_cleanup_aux_temporal(client, config, &config.default_input)?;
        }
        TelegramCommand::BeginProcessReport => {
            let label = super::auto_process_label();
            send_message(
                client,
                config,
                &format!(
                    "Usare la etiqueta automatica: {}\nProcesando con reporte y la entrada por defecto.",
                    label
                ),
            )?;
            let input_path = &config.default_input;
            process_and_reply(client, config, &label, input_path, true)?;
        }
        TelegramCommand::BeginFillPaths => {
            set_pending_action(message.chat.id, PendingAction::FillPathsField);
            send_fill_paths_field_menu(
                client,
                config,
                "Elige el campo Oracle por el que quieres llenar PATH_DIRECTORIOS.txt.",
            )?;
        }
        TelegramCommand::BeginRestore => {
            set_pending_action(message.chat.id, PendingAction::Restore);
            send_message(
                client,
                config,
                "Escribe la etiqueta o la ruta del manifest/respaldo que quieres restaurar.",
            )?;
        }
        TelegramCommand::BeginReport => {
            set_pending_action(message.chat.id, PendingAction::Report);
            send_message(
                client,
                config,
                "Escribe la etiqueta o la ruta del manifest/respaldo para generar el reporte.",
            )?;
        }
        TelegramCommand::Process { label, input } => {
            let input_path = input.as_deref().unwrap_or(&config.default_input);
            process_and_reply(client, config, &label, input_path, false)?;
        }
        TelegramCommand::ProcessReport { label, input } => {
            let input_path = input.as_deref().unwrap_or(&config.default_input);
            process_and_reply(client, config, &label, input_path, true)?;
        }
        TelegramCommand::Restore { target } => {
            send_message(client, config, &format!("Restaurando {} ...", target))?;
            super::restore_from_backup(&target)?;
            let manifest_path = super::manifest_path_from_target(&target)?;
            send_message(
                client,
                config,
                &format!(
                    "Restauracion completada.\nManifest: {}",
                    manifest_path.display()
                ),
            )?;
        }
        TelegramCommand::Report { target } => {
            send_message(
                client,
                config,
                &format!("Generando reporte para {} ...", target),
            )?;
            super::generate_html_report(&target)?;
            let report_path = super::report_path_from_target(&target)?;
            send_document(client, config, &report_path, "Reporte HTML generado")?;
        }
        TelegramCommand::FillPaths { field, value } => {
            if let Some(value) = value {
                fill_paths_from_telegram(client, config, &field, &value)?;
            } else {
                set_pending_action(
                    message.chat.id,
                    PendingAction::FillPathsValue {
                        field: field.clone(),
                    },
                );
                send_message(
                    client,
                    config,
                    &format!(
                        "Campo seleccionado: {}\nAhora escribe el valor de filtro, por ejemplo 16364.",
                        field
                    ),
                )?;
            }
        }
        TelegramCommand::CleanupAuxTemporal { input } => {
            let input_path = input.as_deref().unwrap_or(&config.default_input);
            begin_cleanup_aux_temporal(client, config, input_path)?;
        }
        TelegramCommand::Check { target, folder } => {
            let report = super::build_folder_verification_report(&target, &folder)?;
            send_message(client, config, &report)?;
        }
        TelegramCommand::Unknown(text) => {
            send_message(
                client,
                config,
                &format!("Comando no reconocido: {}\n\n{}", text, help_text()),
            )?;
        }
    }

    Ok(())
}

fn process_and_reply(
    client: &Client,
    config: &TelegramConfig,
    label: &str,
    input: &Path,
    with_report: bool,
) -> Result<(), Box<dyn Error>> {
    send_message(
        client,
        config,
        &format!(
            "Procesando etiqueta {} con {}\nTe avisare cada 30 segundos mientras siga corriendo.",
            label,
            input.display()
        ),
    )?;

    let progress_tracker = TelegramProgressTracker::new();
    let heartbeat = TelegramHeartbeat::start(
        client.clone(),
        config.clone(),
        label.to_string(),
        input.to_path_buf(),
        progress_tracker.current_folder(),
        progress_tracker.total_folders(),
    );

    let process_result = super::run_process_mode(input, label, Some(&progress_tracker));
    drop(heartbeat);
    process_result?;

    let cleanup_result = cleanup_aux_temporal_after_process(client, config, input)?;

    let manifest_path = super::manifest_path_from_target(label)?;
    let manifest = super::read_manifest(&manifest_path)?;
    let execution_time = match (
        manifest
            .header
            .timestamp
            .as_deref()
            .and_then(parse_unix_timestamp),
        manifest
            .header
            .completed_at
            .as_deref()
            .and_then(parse_unix_timestamp),
    ) {
        (Some(start), Some(end)) if end >= start => super::format_duration_seconds(end - start),
        _ => "no registrado".to_string(),
    };
    let source_total_lines = manifest
        .stats
        .lineas_totales
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let source_useful_lines = manifest
        .stats
        .registros_utiles
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let source_valid_directories = manifest
        .stats
        .carpetas_validas
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let source_duplicate_directories = manifest
        .stats
        .duplicados_omitidos
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let source_errors = manifest
        .stats
        .errores
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let attempted_folders = manifest
        .stats
        .carpetas_intentadas
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let processed_folders = manifest
        .stats
        .carpetas_procesadas
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let merged_groups = manifest
        .stats
        .grupos_consolidados
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let pdf_candidates = manifest
        .stats
        .pdfs_candidatos
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let backup_root = manifest
        .header
        .backup_root
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "no registrado".to_string());

    let summary = format!(
        "Proceso terminado para {label}\n\
         Entrada: {}\n\
         Manifest: {}\n\
         Resumen TXT: lineas_totales={}, registros_utiles={}, carpetas_validas={}, duplicados_omitidos={}, errores={}\n\
         Resumen carpetas: intentadas={}, procesadas={}\n\
         Resumen PDFs: grupos_consolidados={}, pdfs_candidatos={}\n\
         Duracion: {}\n\
         Respaldo: {}",
        input.display(),
        manifest_path.display(),
        source_total_lines,
        source_useful_lines,
        source_valid_directories,
        source_duplicate_directories,
        source_errors,
        attempted_folders,
        processed_folders,
        merged_groups,
        pdf_candidates,
        execution_time,
        backup_root,
    );

    send_message(client, config, &summary)?;
    send_message(
        client,
        config,
        &format!(
            "Limpieza automatica de .he1_aux_temporal: borradas={}, no_encontradas={}, errores={}",
            cleanup_result.deleted, cleanup_result.missing, cleanup_result.errors
        ),
    )?;

    if with_report {
        super::generate_html_report(label)?;
        let report_path = super::report_path_from_target(label)?;
        send_document(
            client,
            config,
            &report_path,
            &format!("Reporte HTML de {}", label),
        )?;
    }

    Ok(())
}

fn parse_command(text: &str) -> Result<TelegramCommand, Box<dyn Error>> {
    let trimmed = text.trim();
    match trimmed {
        "1. Preparar directorios" | "Preparar directorios" | "1" => {
            return Ok(TelegramCommand::BeginFillPaths);
        }
        "2. Procesar PDFs" | "Procesar PDFs" | "2" => {
            return Ok(TelegramCommand::BeginProcess);
        }
        "Principal" | "Menu" | "MENÚ" | "Menu principal" | "Menú principal" => {
            return Ok(TelegramCommand::Help);
        }
        _ => {}
    }

    let mut parts = trimmed.split_whitespace();
    let Some(raw_command) = parts.next() else {
        return Err("mensaje vacio".into());
    };
    let command = normalize_command(raw_command);

    let parsed = match command {
        "/start" | "/help" | "/ayuda" => TelegramCommand::Help,
        "/exit" | "/salir" | "Salir" => TelegramCommand::Exit,
        "/mas" | "Mas" | "MAS" | "/avanzado" | "Avanzado" | "MENU" => {
            TelegramCommand::ShowAdvancedMenu
        }
        "/cleanup_temp"
        | "/cleanup-temporal"
        | "/limpiar_temporales"
        | "/limpiar_temporal"
        | "/limpiar"
        | "3. Limpiar temporales"
        | "Limpiar temporales"
        | "3" => match parts.next() {
            Some(input) => TelegramCommand::CleanupAuxTemporal {
                input: Some(PathBuf::from(input)),
            },
            None => TelegramCommand::BeginCleanupAuxTemporal,
        },
        "/process"
        | "/procesar"
        | "PROCESS"
        | "Process"
        | "2. Procesar PDFs"
        | "Procesar PDFs"
        | "2" => {
            match parts.next() {
            Some(label) => TelegramCommand::Process {
                label: label.to_string(),
                input: parts.next().map(PathBuf::from),
            },
            None => TelegramCommand::BeginProcess,
        }
        }
        "/process_report" | "/process-report" | "/process+report" | "/procesar_reporte" => {
            match parts.next() {
                Some(label) => TelegramCommand::ProcessReport {
                    label: label.to_string(),
                    input: parts.next().map(PathBuf::from),
                },
                None => TelegramCommand::BeginProcessReport,
            }
        }
        "/fp"
        | "/fill_paths"
        | "/fill-paths"
        | "/llenar_paths"
        | "/llenar"
        | "fp"
        | "FP"
        | "1. Preparar directorios"
        | "Preparar directorios"
        | "1" => match parts.next() {
                Some(field) => TelegramCommand::FillPaths {
                    field: field.to_string(),
                    value: parts.next().map(|value| value.to_string()),
                },
                None => TelegramCommand::BeginFillPaths,
            },
        "/restore" | "/restaurar" => match parts.next() {
            Some(target) => TelegramCommand::Restore {
                target: target.to_string(),
            },
            None => TelegramCommand::BeginRestore,
        },
        "/report" | "/reporte" | "/html" => match parts.next() {
            Some(target) => TelegramCommand::Report {
                target: target.to_string(),
            },
            None => TelegramCommand::BeginReport,
        },
        "/check" | "/verificar" => {
            let target = parts
                .next()
                .ok_or("uso: /check <etiqueta | ruta_manifest_o_respaldo> <carpeta>")?
                .to_string();
            let folder = parts
                .next()
                .ok_or("uso: /check <etiqueta | ruta_manifest_o_respaldo> <carpeta>")?
                .to_string();
            TelegramCommand::Check {
                target,
                folder: PathBuf::from(folder),
            }
        }
        other if other.starts_with('/') => TelegramCommand::Unknown(other.to_string()),
        _ => return Err("el mensaje no es un comando de Telegram".into()),
    };

    Ok(parsed)
}

fn normalize_command(command: &str) -> &str {
    command.split('@').next().unwrap_or(command)
}

fn is_cancel_text(text: &str) -> bool {
    let normalized = normalize_command(text);
    matches!(normalized, "/exit" | "/salir") || text == "Salir"
}

fn fill_paths_binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("go_aplicacion/bin/path_directorios_fill")
}

fn fill_paths_output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuentes_txt/PATH_DIRECTORIOS.txt")
}

fn validate_ident(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("campo vacio".to_string());
    }

    for ch in value.chars() {
        if ch == '_' || ch == '$' || ch == '#' || ch == '.' {
            continue;
        }
        if ch.is_ascii_alphanumeric() {
            continue;
        }
        return Err(format!("caracter invalido {}", ch));
    }

    Ok(())
}

fn fill_paths_from_telegram(
    client: &Client,
    config: &TelegramConfig,
    field: &str,
    value: &str,
) -> Result<(), Box<dyn Error>> {
    let binary = fill_paths_binary_path();
    if !binary.exists() {
        return Err(format!("no existe el binario esperado: {}", binary.display()).into());
    }

    let output_path = fill_paths_output_path();
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    send_message(
        client,
        config,
        &format!(
            "Llenando PATH_DIRECTORIOS.txt con {} = {}\nEsto puede tardar un poco.",
            field, value
        ),
    )?;

    let output = Command::new(&binary)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("ORACLE_FILTER_FIELD", field)
        .env("ORACLE_FILTER_VALUE", value)
        .env(
            "SQLITE_DSN",
            env::var("SQLITE_DSN")
                .unwrap_or_else(|_| "file:/data_nuevo/repo_grande/data/folders.sqlite".to_string()),
        )
        .env("PATH_DIRECTORIOS_OUT", &output_path)
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        let mut details = String::new();
        if !stdout.trim().is_empty() {
            details.push_str(stdout.trim());
        }
        if !stderr.trim().is_empty() {
            if !details.is_empty() {
                details.push('\n');
            }
            details.push_str(stderr.trim());
        }
        if details.is_empty() {
            details = "sin salida adicional".to_string();
        }
        return Err(format!("no se pudo llenar PATH_DIRECTORIOS.txt:\n{}", details).into());
    }

    let meta_path = output_path.with_extension("meta.txt");
    let mut summary = format!(
        "PATH_DIRECTORIOS.txt actualizado.\nCampo: {}\nValor: {}\n{}",
        field,
        value,
        stdout.trim()
    );
    if !stderr.trim().is_empty() {
        summary.push_str("\nstderr:\n");
        summary.push_str(stderr.trim());
    }
    summary.push_str(&format!(
        "\narchivo escrito en: {}\nmeta escrita en: {}",
        output_path.display(),
        meta_path.display()
    ));

    send_message(client, config, &summary)?;
    send_main_menu_message(
        client,
        config,
        "Directorio preparado. Siguiente paso recomendado: 2. Procesar PDFs con la etiqueta que ya tengas definida. La limpieza queda para el paso 3.",
    )?;
    Ok(())
}

fn begin_cleanup_aux_temporal(
    client: &Client,
    config: &TelegramConfig,
    input_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let targets = collect_cleanup_aux_temporal_targets(input_path)?;

    if targets.is_empty() {
        send_main_menu_message(
            client,
            config,
            &format!(
                "No encontre carpetas con .he1_aux_temporal dentro de {}.\nPuedes seguir con 2. Procesar PDFs.",
                input_path.display()
            ),
        )?;
        return Ok(());
    }

    let preview = targets
        .iter()
        .take(8)
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");

    send_message(
        client,
        config,
        &format!(
            "Encontradas {} carpetas para limpiar.\nSolo se borrara el hijo exacto .he1_aux_temporal dentro de cada ruta.\n\nEjemplos:\n{}\n\nEscribe BORRAR para confirmar o Salir para cancelar.",
            targets.len(),
            preview
        ),
    )?;
    set_pending_action(
        config.chat_id,
        PendingAction::CleanupAuxTemporalConfirm { targets },
    );
    Ok(())
}

fn collect_cleanup_aux_temporal_targets(input_path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let content = fs::read_to_string(input_path)?;
    let mut targets = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let base = PathBuf::from(line);
        if base.file_name().and_then(|name| name.to_str()) == Some(".he1_aux_temporal") {
            continue;
        }

        let target = base.join(".he1_aux_temporal");
        if target.is_dir() {
            targets.push(target);
        }
    }

    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn cleanup_aux_temporal_after_process(
    client: &Client,
    config: &TelegramConfig,
    input_path: &Path,
) -> Result<CleanupResult, Box<dyn Error>> {
    let targets = collect_cleanup_aux_temporal_targets(input_path)?;
    if targets.is_empty() {
        return Ok(CleanupResult::default());
    }

    let result = remove_cleanup_aux_temporal_targets(targets);
    send_message(
        client,
        config,
        &format!(
            "Limpieza automatica de .he1_aux_temporal terminada.\nBorradas: {}\nNo encontradas: {}\nErrores: {}",
            result.deleted, result.missing, result.errors
        ),
    )?;
    Ok(result)
}

fn remove_cleanup_aux_temporal_targets(targets: Vec<PathBuf>) -> CleanupResult {
    let mut result = CleanupResult::default();

    for target in targets {
        if !target.exists() {
            result.missing += 1;
            continue;
        }

        match fs::remove_dir_all(&target) {
            Ok(_) => result.deleted += 1,
            Err(err) => {
                result.errors += 1;
                eprintln!("error al borrar {}: {}", target.display(), err);
            }
        }
    }

    result
}

fn send_fill_paths_field_menu(
    client: &Client,
    config: &TelegramConfig,
    text: &str,
) -> Result<(), Box<dyn Error>> {
    let reply_markup = json!({
        "keyboard": [
            [
                { "text": "DIG_ID_GENERACION" },
                { "text": "DIG_ID_TRAMITE" }
            ],
            [
                { "text": "DIG_TRAMITE" },
                { "text": "Salir" }
            ],
            [
                { "text": "/help" }
            ]
        ],
        "resize_keyboard": true,
        "one_time_keyboard": false,
        "selective": false
    });

    send_message_with_markup(client, config, text, reply_markup)
}

fn get_updates(
    client: &Client,
    config: &TelegramConfig,
    offset: i64,
) -> Result<Vec<Update>, Box<dyn Error>> {
    let response = client
        .get(config.api_url("getUpdates"))
        .query(&[
            ("timeout", POLL_TIMEOUT_SECONDS.to_string()),
            ("offset", offset.to_string()),
        ])
        .send()?
        .error_for_status()?;
    let payload: TelegramResponse<Vec<Update>> = response.json()?;
    decode_telegram_response(payload)
}

fn send_message(
    client: &Client,
    config: &TelegramConfig,
    text: &str,
) -> Result<(), Box<dyn Error>> {
    send_message_with_markup(client, config, text, serde_json::Value::Null)
}

fn send_message_with_markup(
    client: &Client,
    config: &TelegramConfig,
    text: &str,
    reply_markup: serde_json::Value,
) -> Result<(), Box<dyn Error>> {
    let mut form = vec![
        ("chat_id", config.chat_id.to_string()),
        ("text", text.to_string()),
    ];
    if !reply_markup.is_null() {
        form.push(("reply_markup", reply_markup.to_string()));
    }

    let response = client
        .post(config.api_url("sendMessage"))
        .form(&form)
        .send()?
        .error_for_status()?;
    let payload: TelegramResponse<serde_json::Value> = response.json()?;
    let _ = decode_telegram_response(payload)?;
    Ok(())
}

fn send_document(
    client: &Client,
    config: &TelegramConfig,
    path: &Path,
    caption: &str,
) -> Result<(), Box<dyn Error>> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("documento.html")
        .to_string();
    let part = multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str("text/html; charset=utf-8")?;
    let form = multipart::Form::new()
        .text("chat_id", config.chat_id.to_string())
        .text("caption", caption.to_string())
        .part("document", part);

    let response = client
        .post(config.api_url("sendDocument"))
        .multipart(form)
        .send()?
        .error_for_status()?;
    let payload: TelegramResponse<serde_json::Value> = response.json()?;
    let _ = decode_telegram_response(payload)?;
    Ok(())
}

fn decode_telegram_response<T>(payload: TelegramResponse<T>) -> Result<T, Box<dyn Error>> {
    if payload.ok {
        payload
            .result
            .ok_or_else(|| "respuesta de Telegram sin resultado".into())
    } else {
        let code = payload
            .error_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "desconocido".to_string());
        let description = payload
            .description
            .unwrap_or_else(|| "sin descripcion".to_string());
        Err(format!("Telegram error {}: {}", code, description).into())
    }
}

fn send_main_menu_message(
    client: &Client,
    config: &TelegramConfig,
    text: &str,
) -> Result<(), Box<dyn Error>> {
    let reply_markup = json!({
        "keyboard": [
            [
                { "text": "1. Preparar directorios" },
                { "text": "2. Procesar PDFs" }
            ],
            [
                { "text": "Mas" },
                { "text": "/help" }
            ],
            [
                { "text": "Salir" }
            ]
        ],
        "resize_keyboard": true,
        "one_time_keyboard": false,
        "selective": false
    });

    let response = client
        .post(config.api_url("sendMessage"))
        .form(&[
            ("chat_id", config.chat_id.to_string()),
            ("text", text.to_string()),
            ("reply_markup", reply_markup.to_string()),
        ])
        .send()?
        .error_for_status()?;
    let payload: TelegramResponse<serde_json::Value> = response.json()?;
    let _ = decode_telegram_response(payload)?;
    Ok(())
}

fn send_advanced_menu_message(
    client: &Client,
    config: &TelegramConfig,
    text: &str,
) -> Result<(), Box<dyn Error>> {
    let reply_markup = json!({
        "keyboard": [
            [
                { "text": "/process_report" },
                { "text": "/report" }
            ],
            [
                { "text": "/restore" },
                { "text": "Principal" }
            ],
            [
                { "text": "/help" },
                { "text": "Salir" }
            ]
        ],
        "resize_keyboard": true,
        "one_time_keyboard": false,
        "selective": false
    });

    let response = client
        .post(config.api_url("sendMessage"))
        .form(&[
            ("chat_id", config.chat_id.to_string()),
            ("text", text.to_string()),
            ("reply_markup", reply_markup.to_string()),
        ])
        .send()?
        .error_for_status()?;
    let payload: TelegramResponse<serde_json::Value> = response.json()?;
    let _ = decode_telegram_response(payload)?;
    Ok(())
}

fn help_text() -> String {
    vec![
        "Flujo principal: 1. Preparar directorios -> 2. Procesar PDFs".to_string(),
        String::new(),
        "Comandos disponibles:".to_string(),
        "1. Preparar directorios".to_string(),
        "2. Procesar PDFs <etiqueta> [ruta_input]".to_string(),
        "Mas".to_string(),
        "/process_report <etiqueta> [ruta_input]".to_string(),
        "/report <etiqueta | ruta_manifest_o_respaldo>".to_string(),
        "/restore <etiqueta | ruta_manifest_o_respaldo>".to_string(),
        "/fp <campo> [valor]".to_string(),
        "/check <etiqueta | ruta_manifest_o_respaldo> <carpeta>".to_string(),
        "/help".to_string(),
        "".to_string(),
        "Notas:".to_string(),
        "- El orden normal es: 1. Preparar directorios y luego 2. Procesar PDFs.".to_string(),
        "- Al terminar el paso 2, el bot limpia automaticamente los .he1_aux_temporal del listado generado por Go.".to_string(),
        "- Si tocas 2. Procesar PDFs sin escribir nada, el bot genera una etiqueta automatica tipo 202607282036.".to_string(),
        "- /process usa HE1_INPUT si no indicas otra ruta.".to_string(),
        "- /fp llena PATH_DIRECTORIOS.txt usando Oracle y SQLite desde este equipo.".to_string(),
        "- /fp te deja elegir el campo Oracle y luego escribir el valor de filtro.".to_string(),
        "- El bot envia avances resumidos por carpeta durante el proceso.".to_string(),
        "- El bot solo responde al chat configurado en TELEGRAM_CHAT_ID.".to_string(),
        "- El HTML se envia como archivo adjunto cuando se usa /process_report o /report.".to_string(),
        "- /check te dice desde Telegram si una carpeta aparece en el manifest y si conserva el marcador .he1_procesado.".to_string(),
    ]
    .join("\n")
}

fn pending_actions() -> &'static Mutex<HashMap<i64, PendingAction>> {
    static PENDING_ACTIONS: OnceLock<Mutex<HashMap<i64, PendingAction>>> = OnceLock::new();
    PENDING_ACTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_pending_action(chat_id: i64, action: PendingAction) {
    if let Ok(mut guard) = pending_actions().lock() {
        guard.insert(chat_id, action);
    }
}

fn clear_pending_action(chat_id: i64) {
    if let Ok(mut guard) = pending_actions().lock() {
        guard.remove(&chat_id);
    }
}

fn take_pending_action(chat_id: i64) -> Option<PendingAction> {
    pending_actions()
        .lock()
        .ok()
        .and_then(|mut guard| guard.remove(&chat_id))
}

fn parse_unix_timestamp(value: &str) -> Option<i64> {
    value.parse::<i64>().ok()
}
