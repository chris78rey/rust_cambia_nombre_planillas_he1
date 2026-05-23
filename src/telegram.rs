use reqwest::blocking::{multipart, Client};
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const POLL_TIMEOUT_SECONDS: u64 = 30;
const RETRY_DELAY_SECONDS: u64 = 5;

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
    Unknown(String),
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let config = TelegramConfig::from_env()?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mut offset: i64 = 0;

    eprintln!(
        "telegram activo: chat_id={}, input por defecto={}",
        config.chat_id,
        config.default_input.display()
    );

    loop {
        match get_updates(&client, &config, offset) {
            Ok(updates) => {
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
                thread::sleep(Duration::from_secs(RETRY_DELAY_SECONDS));
            }
        }
    }
}

impl TelegramConfig {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        let token = env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| "falta TELEGRAM_BOT_TOKEN en el entorno")?;
        let chat_id = env::var("TELEGRAM_CHAT_ID")
            .map_err(|_| "falta TELEGRAM_CHAT_ID en el entorno")?
            .parse::<i64>()
            .map_err(|_| "TELEGRAM_CHAT_ID debe ser un numero entero")?;
        let default_input = PathBuf::from(
            env::var("HE1_INPUT").map_err(|_| "falta HE1_INPUT en el entorno")?,
        );

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

fn handle_message(
    client: &Client,
    config: &TelegramConfig,
    message: &Message,
) -> Result<(), Box<dyn Error>> {
    let Some(text) = message.text.as_deref() else {
        return Ok(());
    };

    let command = parse_command(text)?;
    match command {
        TelegramCommand::Help => {
            send_message(client, config, &help_text())?;
        }
        TelegramCommand::Process { label, input } => {
            let input_path = input.as_deref().unwrap_or(&config.default_input);
            send_message(
                client,
                config,
                &format!(
                    "Procesando etiqueta {} con {} ...",
                    label,
                    input_path.display()
                ),
            )?;
            process_and_reply(client, config, &label, input_path, false)?;
        }
        TelegramCommand::ProcessReport { label, input } => {
            let input_path = input.as_deref().unwrap_or(&config.default_input);
            send_message(
                client,
                config,
                &format!(
                    "Procesando y generando reporte para {} con {} ...",
                    label,
                    input_path.display()
                ),
            )?;
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
            send_message(client, config, &format!("Generando reporte para {} ...", target))?;
            super::generate_html_report(&target)?;
            let report_path = super::report_path_from_target(&target)?;
            send_document(
                client,
                config,
                &report_path,
                "Reporte HTML generado",
            )?;
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
    super::run_process_mode(input, label)?;

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
    let mut parts = text.split_whitespace();
    let Some(raw_command) = parts.next() else {
        return Err("mensaje vacio".into());
    };
    let command = normalize_command(raw_command);

    let parsed = match command {
        "/start" | "/help" | "/ayuda" => TelegramCommand::Help,
        "/process" | "/procesar" => {
            let label = parts
                .next()
                .ok_or("uso: /process <etiqueta> [ruta_input]")?
                .to_string();
            let input = parts.next().map(PathBuf::from);
            TelegramCommand::Process { label, input }
        }
        "/process_report" | "/process-report" | "/process+report" | "/procesar_reporte" => {
            let label = parts
                .next()
                .ok_or("uso: /process_report <etiqueta> [ruta_input]")?
                .to_string();
            let input = parts.next().map(PathBuf::from);
            TelegramCommand::ProcessReport { label, input }
        }
        "/restore" | "/restaurar" => {
            let target = parts
                .next()
                .ok_or("uso: /restore <etiqueta | ruta_manifest_o_respaldo>")?
                .to_string();
            TelegramCommand::Restore { target }
        }
        "/report" | "/reporte" | "/html" => {
            let target = parts
                .next()
                .ok_or("uso: /report <etiqueta | ruta_manifest_o_respaldo>")?
                .to_string();
            TelegramCommand::Report { target }
        }
        other if other.starts_with('/') => TelegramCommand::Unknown(other.to_string()),
        _ => return Err("el mensaje no es un comando de Telegram".into()),
    };

    Ok(parsed)
}

fn normalize_command(command: &str) -> &str {
    command.split('@').next().unwrap_or(command)
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
    let response = client
        .post(config.api_url("sendMessage"))
        .form(&[
            ("chat_id", config.chat_id.to_string()),
            ("text", text.to_string()),
        ])
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

fn help_text() -> String {
    [
        "Comandos disponibles:",
        "/process <etiqueta> [ruta_input]",
        "/process_report <etiqueta> [ruta_input]",
        "/restore <etiqueta | ruta_manifest_o_respaldo>",
        "/report <etiqueta | ruta_manifest_o_respaldo>",
        "/help",
        "",
        "Notas:",
        "- /process y /process_report usan HE1_INPUT si no indicas otra ruta.",
        "- El bot solo responde al chat configurado en TELEGRAM_CHAT_ID.",
        "- El HTML se envia como archivo adjunto cuando se usa /process_report o /report.",
    ]
    .join("\n")
}

fn parse_unix_timestamp(value: &str) -> Option<i64> {
    value.parse::<i64>().ok()
}
