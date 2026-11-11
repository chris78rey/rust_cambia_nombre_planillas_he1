use lopdf::Document;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::env;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod telegram;

const VALID_NAMES: &[&str] = &[
    "PI", "CC", "CV", "AES", "053", "006", "007", "017", "018", "018A", "113", "114", "115", "ORS",
    "002", "010A", "010B", "012A", "012B", "033", "013A", "013B", "PTR", "RTR", "08", "008",
    "FSCS", "FSICS", "FRDCS", "ANX2", "HR", "RHD", "IMT", "CEC", "RAD", "ITS", "RVD", "119",
];

const BACKUP_DIR_NAME: &str = "he1_respaldo";
const LABEL_INDEX_DIR: &str = ".he1_label_index";
const BACKUP_RETENTION_SECONDS: u64 = 2 * 24 * 60 * 60;

#[derive(Debug)]
enum AppMode {
    Process { input: PathBuf, label: String },
    Restore(String),
    Report(String),
    Check { target: String, folder: PathBuf },
    ConvertPaths { input: PathBuf, output: PathBuf },
    Objections { label: String, dry_run: bool },
    RetryObjectionRefresh,
    Telegram,
    Help,
}

#[derive(Default)]
struct FolderSummary {
    merged_groups: usize,
    merged_files: usize,
    renamed_files: usize,
}

#[derive(Default)]
struct RunSummary {
    attempted_folders: usize,
    processed_folders: usize,
    merged_groups: usize,
    merged_files: usize,
    renamed_files: usize,
}

#[derive(Debug, Deserialize)]
struct ObjectionDbPlan {
    label: String,
    items: Vec<ObjectionDbPlanItem>,
}

#[derive(Debug, Deserialize)]
struct ObjectionDbPlanItem {
    id_descarga: String,
    tramite: String,
    nombre_pdf: String,
    #[serde(default)]
    folder_paths: Vec<String>,
    #[serde(default)]
    folder_path: Option<String>,
}

#[derive(Debug, Clone)]
struct ObjectionMergePlan {
    tramite: String,
    folder: PathBuf,
    base: String,
    part_paths: Vec<PathBuf>,
    unrequested_part_paths: Vec<PathBuf>,
    cleanup_paths: Vec<PathBuf>,
    input_paths: Vec<PathBuf>,
    source_snapshots: Vec<ObjectionSourceSnapshot>,
    unrequested_snapshots: Vec<ObjectionSourceSnapshot>,
    output_path: PathBuf,
    total_pages: u32,
    db_groups: Vec<ObjectionDbChange>,
}

#[derive(Debug, Clone, Serialize)]
struct ObjectionSourceSnapshot {
    filename: String,
    path: String,
    size_bytes: u64,
    pages: u32,
    modified_unix_seconds: Option<u64>,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct ObjectionPreviewReport {
    label: String,
    groups: Vec<ObjectionPreviewGroup>,
}

#[derive(Debug, Clone, Serialize)]
struct ObjectionPreviewGroup {
    tramite: String,
    document: String,
    destination: String,
    total_pages: u32,
    oracle_items: Vec<ObjectionPreviewOracleItem>,
    source_files: Vec<ObjectionSourceSnapshot>,
    unrequested_files: Vec<ObjectionSourceSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
struct ObjectionPreviewOracleItem {
    id_descarga: String,
    source_names: Vec<String>,
}

#[derive(Debug)]
struct ObjectionExecutionPlan {
    helper: PathBuf,
    plans: Vec<ObjectionMergePlan>,
    report: ObjectionPreviewReport,
    fingerprint: String,
}

#[derive(Debug)]
pub(crate) struct TelegramObjectionPreview {
    pub(crate) label: String,
    pub(crate) fingerprint: String,
    pub(crate) report_path: PathBuf,
    pub(crate) groups: usize,
    pub(crate) pages: u64,
}

#[derive(Debug)]
pub(crate) enum TelegramObjectionApply {
    Applied {
        summary: String,
        manifest_path: PathBuf,
        report_path: PathBuf,
    },
    PlanChanged(TelegramObjectionPreview),
}

#[derive(Debug)]
struct ObjectionApplyOutcome {
    summary: String,
    manifest_path: PathBuf,
    report_path: PathBuf,
}

enum ObjectionRunOutcome {
    NoGroups,
    DryRun,
    PlanChanged(TelegramObjectionPreview),
    Applied(ObjectionApplyOutcome),
}

#[derive(Debug, Clone, Serialize)]
struct ObjectionDbChange {
    id_descarga: String,
    tramite: String,
    base: String,
    canonical_name: String,
    part_names: Vec<String>,
    output_path: String,
    total_pages: u32,
}

#[derive(Debug, Serialize)]
struct ObjectionDbApplyRequest {
    label: String,
    groups: Vec<ObjectionDbChange>,
}

#[derive(Debug, Deserialize)]
struct ObjectionDbApplyResponse {
    status: String,
    message: Option<String>,
    groups_updated: usize,
    items_deleted: i64,
    downloads_updated: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingObjectionRefresh {
    label: String,
    tramite: String,
}

#[derive(Debug, Clone, Serialize)]
struct ObjectionJournal {
    operation_id: String,
    label: String,
    plan_fingerprint: String,
    pre_merge_report: String,
    pre_merge_report_sha256: String,
    state: String,
    groups: Vec<ObjectionJournalGroup>,
}

#[derive(Debug, Clone, Serialize)]
struct ObjectionJournalGroup {
    tramite: String,
    base: String,
    output_path: String,
    temporary_path: String,
    part_paths: Vec<String>,
    unrequested_part_paths: Vec<String>,
    cleanup_paths: Vec<String>,
    input_paths: Vec<String>,
    source_snapshots: Vec<ObjectionSourceSnapshot>,
    unrequested_snapshots: Vec<ObjectionSourceSnapshot>,
    previous_output_backup: Option<String>,
    total_pages: u32,
    output_size_bytes: u64,
    output_sha256: String,
}

#[derive(Debug)]
struct ObjectionFileMutation {
    output_path: PathBuf,
    temporary_path: PathBuf,
    previous_output_backup: Option<PathBuf>,
    cleanup_paths: Vec<PathBuf>,
}

#[derive(Default)]
struct ObjectionTempCleanup(Vec<PathBuf>);

impl ObjectionTempCleanup {
    fn track(&mut self, path: PathBuf) {
        self.0.push(path);
    }
}

impl Drop for ObjectionTempCleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

pub trait ProgressSink {
    fn note(&self, message: &str);

    fn update_folder_progress(&self, _current: usize, _total: usize) {}
}

fn emit_progress(progress: Option<&dyn ProgressSink>, message: String) {
    if let Some(progress) = progress {
        progress.note(&message);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let mode = parse_args()?;
    if !matches!(
        &mode,
        AppMode::Objections { dry_run: true, .. } | AppMode::RetryObjectionRefresh
    ) {
        if let Err(err) = cleanup_expired_backups() {
            eprintln!(
                "advertencia: no se pudo aplicar la retencion de respaldos: {}",
                err
            );
        }
    }
    let result = match &mode {
        AppMode::Help => {
            print_usage();
            Ok(())
        }
        AppMode::Restore(target) => restore_from_backup(target),
        AppMode::Report(target) => generate_html_report(target),
        AppMode::Check { target, folder } => verify_folder_processed(target, folder),
        AppMode::ConvertPaths { input, output } => convert_directory_list_paths(input, output),
        AppMode::Objections { label, dry_run } => run_objection_mode(label, *dry_run),
        AppMode::RetryObjectionRefresh => retry_pending_objection_refreshes(),
        AppMode::Telegram => telegram::run(),
        AppMode::Process { input, label } => run_process_mode(input, label, None),
    };

    if let Err(err) = &result {
        eprintln!("error de ejecucion: {}", err);
        if matches!(mode, AppMode::Telegram) {
            let _ = telegram::send_operator_notice_from_env(&format!(
                "Bot de Telegram detenido con error:\n{}",
                err
            ));
        }
    }

    result
}

fn parse_args() -> Result<AppMode, Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(AppMode::Help);
    }

    if args.first().map(String::as_str) == Some("--modo") {
        if args.get(1).map(String::as_str) != Some("objeciones") {
            return Err("modo no válido; opciones: objeciones".into());
        }
        let mut label: Option<String> = None;
        let mut dry_run = false;
        let mut retry_refresh = false;
        let mut index = 2;
        while index < args.len() {
            match args[index].as_str() {
                "--label" => {
                    index += 1;
                    let value = args.get(index).ok_or("falta valor para --label")?;
                    label = Some(value.clone());
                }
                "--dry-run" => dry_run = true,
                "--reintentar-refresh" | "--retry-refresh" => retry_refresh = true,
                unknown => {
                    return Err(format!("opción desconocida para objeciones: {}", unknown).into());
                }
            }
            index += 1;
        }
        if retry_refresh {
            if label.is_some() || dry_run {
                return Err("--reintentar-refresh se usa sin --label ni --dry-run".into());
            }
            return Ok(AppMode::RetryObjectionRefresh);
        }
        let label = label.ok_or("falta --label <etiqueta_objecion>")?;
        validate_backup_label(&label)?;
        return Ok(AppMode::Objections { label, dry_run });
    }

    if args.first().map(|s| s.as_str()) == Some("--telegram") {
        if args.len() != 1 {
            return Err("uso: he1-unificar-pdfs --telegram".into());
        }
        return Ok(AppMode::Telegram);
    }

    if args.first().map(|s| s.as_str()) == Some("--restore") {
        if args.len() != 2 {
            return Err(
                "uso: he1-unificar-pdfs --restore <etiqueta | ruta_respaldo_o_manifest.txt>".into(),
            );
        }
        return Ok(AppMode::Restore(args[1].clone()));
    }

    if matches!(
        args.first().map(|s| s.as_str()),
        Some("--check" | "--verificar")
    ) {
        if args.len() != 3 {
            return Err(
                "uso: he1-unificar-pdfs --check <etiqueta | ruta_manifest_o_respaldo> <carpeta>"
                    .into(),
            );
        }
        return Ok(AppMode::Check {
            target: args[1].clone(),
            folder: PathBuf::from(&args[2]),
        });
    }

    if matches!(
        args.first().map(|s| s.as_str()),
        Some("--report" | "--reporte" | "--html")
    ) {
        if args.len() != 2 {
            return Err(
                "uso: he1-unificar-pdfs --report <etiqueta | ruta_manifest_o_respaldo>".into(),
            );
        }
        return Ok(AppMode::Report(args[1].clone()));
    }

    if matches!(
        args.first().map(|s| s.as_str()),
        Some("--convert-paths" | "--convertir-rutas" | "--windows-to-linux")
    ) {
        if args.len() != 3 {
            return Err("uso: he1-unificar-pdfs --convert-paths <entrada.txt> <salida.txt>".into());
        }
        return Ok(AppMode::ConvertPaths {
            input: PathBuf::from(&args[1]),
            output: PathBuf::from(&args[2]),
        });
    }

    if args.first().map(|s| s.as_str()) == Some("--label") {
        if args.len() != 3 {
            return Err("uso: he1-unificar-pdfs --label <etiqueta> <ruta.txt | carpeta>".into());
        }
        return Ok(AppMode::Process {
            input: PathBuf::from(&args[2]),
            label: args[1].clone(),
        });
    }

    Err("uso: he1-unificar-pdfs --label <etiqueta> <ruta.txt | carpeta>".into())
}

fn run_process_mode(
    input: &Path,
    label: &str,
    progress: Option<&dyn ProgressSink>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !input.exists() {
        return Err(format!("ruta no existe: {}", input.display()).into());
    }

    if input.is_file()
        && input
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("txt"))
            == Some(true)
    {
        run_directory_list(input, Some(label), progress)
    } else if input.is_dir() {
        run_directories(
            vec![input.to_path_buf()],
            input,
            Some(label),
            &[],
            None,
            progress,
        )
    } else {
        Err(format!(
            "la entrada debe ser una carpeta o un archivo .txt: {}",
            input.display()
        )
        .into())
    }
}

fn run_objection_mode(label: &str, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    let _ = run_objection_mode_checked(label, dry_run, None)?;
    Ok(())
}

fn run_objection_mode_checked(
    label: &str,
    dry_run: bool,
    expected_fingerprint: Option<&str>,
) -> Result<ObjectionRunOutcome, Box<dyn std::error::Error>> {
    let execution = build_objection_execution_plan(label)?;
    let helper = execution.helper.clone();
    let plans = execution.plans;
    let report = execution.report;
    let fingerprint = execution.fingerprint;
    if plans.is_empty() {
        println!(
            "La etiqueta {} no contiene grupos PDF fraccionados elegibles.",
            label
        );
        return Ok(ObjectionRunOutcome::NoGroups);
    }

    if expected_fingerprint.is_some_and(|expected| expected != fingerprint) {
        let preview = write_telegram_objection_preview(label, &report, &fingerprint)?;
        return Ok(ObjectionRunOutcome::PlanChanged(preview));
    }

    println!(
        "Objeción {}: {} grupo(s) para consolidar",
        label,
        plans.len()
    );
    for plan in &plans {
        println!(
            "  trámite {} | {} | {} parte(s) + PDF canónico existente si aplica | {} página(s) -> {}",
            plan.tramite,
            plan.base,
            plan.part_paths.len(),
            plan.total_pages,
            plan.output_path.display()
        );
        println!("    partes: {}", describe_files(&plan.part_paths));
        if !plan.unrequested_part_paths.is_empty() {
            println!(
                "    partes fuera de esta etiqueta (se dejan intactas): {}",
                describe_files(&plan.unrequested_part_paths)
            );
        }
    }
    if dry_run {
        println!("Simulación: no se modificaron PDFs, Oracle ni caché.");
        return Ok(ObjectionRunOutcome::DryRun);
    }

    let refresh_template = objection_refresh_template()?;
    let refresh_client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let project_root = env::current_dir()?;
    let operation_id = objection_operation_id(label);
    let backup_root = project_root
        .join("he1_respaldo")
        .join("objeciones")
        .join(&operation_id);
    fs::create_dir_all(&backup_root)?;
    let pre_merge_report_path = backup_root.join("pre_merge.csv");
    write_objection_preview_csv(&report, &pre_merge_report_path)?;
    let pre_merge_report_sha256 = sha256_file(&pre_merge_report_path)?;

    let mut mutations: Vec<ObjectionFileMutation> = Vec::with_capacity(plans.len());
    let mut temporary_files = ObjectionTempCleanup::default();
    let mut journal = ObjectionJournal {
        operation_id: operation_id.clone(),
        label: label.to_string(),
        plan_fingerprint: fingerprint.clone(),
        pre_merge_report: pre_merge_report_path.display().to_string(),
        pre_merge_report_sha256,
        state: "preparing".to_string(),
        groups: Vec::with_capacity(plans.len()),
    };
    let journal_path = backup_root.join("manifest.json");

    for (index, plan) in plans.iter().enumerate() {
        let group_backup = backup_root.join(format!("grupo_{:04}", index + 1));
        fs::create_dir_all(&group_backup)?;
        let mut staged_inputs = Vec::with_capacity(plan.input_paths.len());
        for (source_index, source) in plan.input_paths.iter().enumerate() {
            let backup = group_backup.join(format!("origen_{:03}.pdf", source_index + 1));
            fs::copy(source, &backup)?;
            let expected = plan
                .source_snapshots
                .iter()
                .find(|snapshot| snapshot.path == source.display().to_string())
                .ok_or_else(|| format!("no hay huella previa para {}", source.display()))?;
            if sha256_file(&backup)? != expected.sha256 {
                return Err(format!(
                    "el archivo cambió después de la vista previa: {}. No se aplicó el plan.",
                    source.display()
                )
                .into());
            }
            staged_inputs.push(backup);
        }
        let previous_output_backup = if plan.output_path.exists() {
            let backup = group_backup.join("salida_anterior.pdf");
            fs::copy(&plan.output_path, &backup)?;
            Some(backup)
        } else {
            None
        };
        let temporary_path = plan
            .folder
            .join(format!(".{}.{}.tmp.pdf", plan.base, operation_id));
        if temporary_path.exists() {
            return Err(format!(
                "ya existe un temporal de objeción: {}",
                temporary_path.display()
            )
            .into());
        }
        temporary_files.track(temporary_path.clone());
        if let Err(err) = merge_pdfs(&staged_inputs, &temporary_path) {
            for mutation in &mutations {
                let _ = fs::remove_file(&mutation.temporary_path);
            }
            let _ = fs::remove_file(&temporary_path);
            return Err(
                format!("no se pudo unir {} en {}: {}", plan.base, plan.tramite, err).into(),
            );
        }
        let merged_pages = page_count(&temporary_path)?;
        let merged_size = fs::metadata(&temporary_path)?.len();
        let merged_sha256 = sha256_file(&temporary_path)?;
        if merged_pages != plan.total_pages || merged_size == 0 {
            let _ = fs::remove_file(&temporary_path);
            for mutation in &mutations {
                let _ = fs::remove_file(&mutation.temporary_path);
            }
            return Err(format!(
                "validación fallida para trámite {} base {}: esperadas {} páginas, salida {} páginas, {} bytes",
                plan.tramite, plan.base, plan.total_pages, merged_pages, merged_size
            )
            .into());
        }
        mutations.push(ObjectionFileMutation {
            output_path: plan.output_path.clone(),
            temporary_path: temporary_path.clone(),
            previous_output_backup: previous_output_backup.clone(),
            cleanup_paths: plan.cleanup_paths.clone(),
        });
        journal.groups.push(ObjectionJournalGroup {
            tramite: plan.tramite.clone(),
            base: plan.base.clone(),
            output_path: plan.output_path.display().to_string(),
            temporary_path: temporary_path.display().to_string(),
            part_paths: plan
                .part_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            unrequested_part_paths: plan
                .unrequested_part_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            cleanup_paths: plan
                .cleanup_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            input_paths: plan
                .input_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            source_snapshots: plan.source_snapshots.clone(),
            unrequested_snapshots: plan.unrequested_snapshots.clone(),
            previous_output_backup: previous_output_backup.map(|p| p.display().to_string()),
            total_pages: plan.total_pages,
            output_size_bytes: merged_size,
            output_sha256: merged_sha256,
        });
    }

    if let Err(err) = write_json_atomic(&journal_path, &journal) {
        for mutation in &mutations {
            let _ = fs::remove_file(&mutation.temporary_path);
        }
        return Err(err);
    }
    let mut published = 0usize;
    for mutation in &mutations {
        if let Err(err) = fs::rename(&mutation.temporary_path, &mutation.output_path) {
            let rollback = rollback_objection_outputs(&mutations[..published]);
            journal.state = if rollback.is_ok() {
                "rolled_back"
            } else {
                "recovery_required"
            }
            .to_string();
            let _ = write_json_atomic(&journal_path, &journal);
            for pending in &mutations[published..] {
                let _ = fs::remove_file(&pending.temporary_path);
            }
            return Err(format!(
                "no se pudo publicar la salida; rollback={:?}: {}",
                rollback.err(),
                err
            )
            .into());
        }
        published += 1;
    }
    journal.state = "files_published".to_string();
    if let Err(err) = write_json_atomic(&journal_path, &journal) {
        let rollback = rollback_objection_outputs(&mutations);
        journal.state = if rollback.is_ok() {
            "rolled_back"
        } else {
            "recovery_required"
        }
        .to_string();
        let _ = write_json_atomic(&journal_path, &journal);
        return Err(format!(
            "no se pudo guardar el estado del manifiesto; rollback={:?}: {}",
            rollback.err(),
            err
        )
        .into());
    }

    let changes = plans
        .iter()
        .flat_map(|plan| plan.db_groups.iter().cloned())
        .collect::<Vec<_>>();
    let apply = ObjectionDbApplyRequest {
        label: label.to_string(),
        groups: changes,
    };
    let response = match run_objection_db_apply(&helper, label, &apply) {
        Ok(response) => response,
        Err(err) => {
            let rollback = rollback_objection_outputs(&mutations);
            journal.state = if rollback.is_ok() {
                "rolled_back"
            } else {
                "recovery_required"
            }
            .to_string();
            let _ = write_json_atomic(&journal_path, &journal);
            return Err(format!(
                "Oracle no confirmó la actualización; rollback={:?}: {}",
                rollback.err(),
                err
            )
            .into());
        }
    };
    if response.status == "commit_uncertain" {
        journal.state = "oracle_commit_uncertain".to_string();
        write_json_atomic(&journal_path, &journal)?;
        return Err(format!(
            "Oracle no confirmó el resultado del commit. Se conservaron los fragmentos y la salida; revisar antes de reintentar. {}",
            response.message.unwrap_or_default()
        )
        .into());
    }
    if response.status != "committed" {
        let rollback = rollback_objection_outputs(&mutations);
        journal.state = if rollback.is_ok() {
            "rolled_back"
        } else {
            "recovery_required"
        }
        .to_string();
        let _ = write_json_atomic(&journal_path, &journal);
        return Err(format!(
            "respuesta Oracle inesperada {:?}; rollback={:?}",
            response.status,
            rollback.err()
        )
        .into());
    }

    journal.state = "oracle_committed".to_string();
    if let Err(err) = write_json_atomic(&journal_path, &journal) {
        eprintln!(
            "advertencia: Oracle confirmó cambios, pero no se actualizó el manifiesto: {}",
            err
        );
    }

    let mut cleanup_errors = Vec::new();
    for mutation in &mutations {
        for source in &mutation.cleanup_paths {
            let expected_hash = plans
                .iter()
                .flat_map(|plan| plan.source_snapshots.iter())
                .find(|snapshot| snapshot.path == source.display().to_string())
                .map(|snapshot| snapshot.sha256.as_str());
            match (expected_hash, sha256_file(source)) {
                (Some(expected), Ok(actual)) if actual == expected => {
                    if let Err(err) = fs::remove_file(source) {
                        cleanup_errors.push(format!("{}: {}", source.display(), err));
                    }
                }
                (Some(_), Ok(_)) => cleanup_errors.push(format!(
                    "{} cambió después de la consolidación; se dejó intacto",
                    source.display()
                )),
                (_, Err(err)) => cleanup_errors.push(format!("{}: {}", source.display(), err)),
                (None, _) => cleanup_errors.push(format!(
                    "{} no tiene huella previa; se dejó intacto",
                    source.display()
                )),
            }
        }
    }

    let mut pending = Vec::new();
    let mut refreshed = HashSet::new();
    for plan in &plans {
        if !refreshed.insert(plan.tramite.clone()) {
            continue;
        }
        match refresh_objection_cache(&refresh_client, &refresh_template, &plan.tramite) {
            Ok(()) => {
                if let Err(err) = remove_pending_refresh(&project_root, label, &plan.tramite) {
                    eprintln!(
                        "advertencia: la caché se actualizó, pero no se pudo limpiar la cola local: {}",
                        err
                    );
                }
            }
            Err(err) => {
                eprintln!(
                    "advertencia: no se pudo refrescar la caché del trámite {}: {}",
                    plan.tramite, err
                );
                pending.push(PendingObjectionRefresh {
                    label: label.to_string(),
                    tramite: plan.tramite.clone(),
                });
            }
        }
    }
    let mut pending_queue_error = None;
    if !pending.is_empty() {
        if let Err(err) = append_pending_refreshes(&project_root, &pending) {
            pending_queue_error = Some(err.to_string());
        }
    }

    journal.state = if !cleanup_errors.is_empty() {
        "cleanup_pending".to_string()
    } else if !pending.is_empty() {
        if pending_queue_error.is_some() {
            "refresh_pending_not_recorded".to_string()
        } else {
            "refresh_pending".to_string()
        }
    } else {
        "completed".to_string()
    };
    write_json_atomic(&journal_path, &journal)?;

    let summary = format!(
        "Objeción {} completada: {} grupo(s) unidos, {} fila(s) eliminadas, {} descarga(s) actualizadas. Estado: {}. Respaldo: {}.",
        label,
        response.groups_updated,
        response.items_deleted,
        response.downloads_updated,
        journal.state,
        backup_root.display()
    );
    println!("{}", summary);
    println!("Respaldo y manifiesto: {}", backup_root.display());
    if !cleanup_errors.is_empty() {
        eprintln!(
            "No se pudieron borrar algunos fragmentos; el PDF canónico y Oracle ya quedaron actualizados:\n  {}",
            cleanup_errors.join("\n  ")
        );
    }
    if !pending.is_empty() {
        eprintln!(
            "Quedó pendiente refrescar la caché. Reintenta con: --modo objeciones --reintentar-refresh"
        );
    }
    if let Some(err) = pending_queue_error {
        eprintln!(
            "No se pudo guardar la cola local de refresco; conserva el trámite y vuelve a ejecutar el refresco: {}",
            err
        );
    }
    Ok(ObjectionRunOutcome::Applied(ObjectionApplyOutcome {
        summary,
        manifest_path: journal_path,
        report_path: pre_merge_report_path,
    }))
}

fn objection_db_helper_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(path) = env::var("HE1_OBJECTION_DB_HELPER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "HE1_OBJECTION_DB_HELPER no apunta a un ejecutable: {}",
            path.display()
        )
        .into());
    }
    if let Ok(executable) = env::current_exe() {
        if let Some(parent) = executable.parent() {
            let sibling = parent.join("he1-objeciones-db");
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    let source_tree =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("go_aplicacion/bin/he1-objeciones-db");
    if source_tree.is_file() {
        return Ok(source_tree);
    }
    Err(format!(
        "no se encontró el auxiliar Oracle {}; compílalo con: cd go_aplicacion && go build -o bin/he1-objeciones-db ./cmd/objeciones",
        source_tree.display()
    )
    .into())
}

fn run_objection_db_plan(
    helper: &Path,
    label: &str,
) -> Result<ObjectionDbPlan, Box<dyn std::error::Error>> {
    let output = Command::new(helper)
        .args(["--action", "plan", "--label", label])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "falló la consulta Oracle: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn run_objection_db_apply(
    helper: &Path,
    label: &str,
    request: &ObjectionDbApplyRequest,
) -> Result<ObjectionDbApplyResponse, Box<dyn std::error::Error>> {
    let mut child = Command::new(helper)
        .args(["--action", "apply", "--label", label])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    serde_json::to_writer(
        child
            .stdin
            .as_mut()
            .ok_or("no se abrió stdin del auxiliar Oracle")?,
        request,
    )?;
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "Oracle rechazó la transacción: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn objection_data_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let configured = env::var("HE1_OBJECTION_DATA_ROOT")
        .unwrap_or_else(|_| "/data_nuevo/repo_grande/data/datos".to_string());
    let root = fs::canonicalize(&configured)
        .map_err(|err| format!("no existe HE1_OBJECTION_DATA_ROOT {}: {}", configured, err))?;
    if !root.is_dir() {
        return Err(format!("HE1_OBJECTION_DATA_ROOT no es carpeta: {}", root.display()).into());
    }
    Ok(root)
}

fn build_objection_merge_plans(
    db_plan: &ObjectionDbPlan,
    allowed_root: &Path,
) -> Result<Vec<ObjectionMergePlan>, Box<dyn std::error::Error>> {
    let mut groups: BTreeMap<(String, String), BTreeMap<String, BTreeSet<String>>> =
        BTreeMap::new();
    let mut candidate_paths: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for item in &db_plan.items {
        let name = Path::new(&item.nombre_pdf)
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| {
                format!(
                    "nombre PDF inválido en Oracle para trámite {}",
                    item.tramite
                )
            })?;
        if name != item.nombre_pdf {
            return Err(format!(
                "Oracle devolvió una ruta en lugar de nombre PDF: {}",
                item.nombre_pdf
            )
            .into());
        }
        let Some((base, is_part)) = classify_objection_pdf(name) else {
            continue;
        };
        if !is_part {
            continue;
        }
        let mut item_paths = item.folder_paths.clone();
        if let Some(path) = &item.folder_path {
            item_paths.push(path.clone());
        }
        if item_paths.is_empty() {
            return Err(
                format!("SQLite no devolvió carpetas para trámite {}", item.tramite).into(),
            );
        }
        let key = (item.tramite.clone(), base);
        groups
            .entry(key.clone())
            .or_default()
            .entry(item.id_descarga.clone())
            .or_default()
            .insert(name.to_ascii_uppercase());
        candidate_paths.entry(key).or_default().extend(item_paths);
    }

    let mut plans = Vec::new();
    for ((tramite, base), by_download) in groups {
        let Some(expected_parts) = by_download.values().next().cloned() else {
            continue;
        };
        if expected_parts.len() < 2 {
            continue;
        }
        for (id_descarga, names) in &by_download {
            if names.len() < 2 || names != &expected_parts {
                return Err(format!(
                    "los archivos fraccionados de {} en la descarga {} no están completos o no coinciden",
                    base, id_descarga
                )
                .into());
            }
        }

        let mut matching_folders = Vec::new();
        let mut folder_diagnostics = Vec::new();
        let raw_candidates = candidate_paths
            .get(&(tramite.clone(), base.clone()))
            .ok_or_else(|| format!("no hay carpetas candidatas para trámite {}", tramite))?;
        let mut canonical_candidates = BTreeSet::new();
        for raw_path in raw_candidates {
            let folder = fs::canonicalize(raw_path).map_err(|err| {
                format!(
                    "ruta inválida para trámite {} ({}): {}",
                    tramite, raw_path, err
                )
            })?;
            if !folder.starts_with(allowed_root) {
                return Err(format!(
                    "ruta fuera de HE1_OBJECTION_DATA_ROOT para trámite {}: {}",
                    tramite,
                    folder.display()
                )
                .into());
            }
            if !folder.is_dir() {
                continue;
            }
            canonical_candidates.insert(folder);
        }
        for folder in canonical_candidates {
            let mut disk_parts: BTreeMap<String, PathBuf> = BTreeMap::new();
            for entry in fs::read_dir(&folder).map_err(|err| {
                format!(
                    "no se pudo leer carpeta candidata {}: {}",
                    folder.display(),
                    err
                )
            })? {
                let path = entry?.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                if let Some((file_base, true)) = classify_objection_pdf(&name) {
                    if file_base == base {
                        let key = name.to_ascii_uppercase();
                        if disk_parts.insert(key.clone(), path).is_some() {
                            return Err(format!(
                                "hay nombres de PDF duplicados ignorando mayúsculas en trámite {}: {}",
                                tramite, name
                            )
                            .into());
                        }
                    }
                }
            }
            let disk_names = disk_parts.keys().cloned().collect::<BTreeSet<_>>();
            if expected_parts.is_subset(&disk_names) {
                matching_folders.push((folder, disk_parts));
            } else {
                folder_diagnostics.push(format!("{} => {:?}", folder.display(), disk_names));
            }
        }
        if matching_folders.len() != 1 {
            let detail = if matching_folders.is_empty() {
                format!(
                    "ninguna carpeta contiene todas las partes de Oracle; Oracle={:?}; disco=[{}]",
                    expected_parts,
                    folder_diagnostics.join("; ")
                )
            } else {
                "más de una carpeta coincide con los fragmentos de Oracle".to_string()
            };
            return Err(format!(
                "no se puede elegir una carpeta única para trámite {} y {}: {}",
                tramite, base, detail
            )
            .into());
        }
        let (folder, disk_parts) = matching_folders.pop().expect("se validó una única carpeta");
        let output_path = folder.join(format!("{}.pdf", base));
        let mut part_paths = expected_parts
            .iter()
            .filter_map(|name| disk_parts.get(name).cloned())
            .collect::<Vec<_>>();
        part_paths.sort_by(|a, b| compare_objection_parts(a, b, &base));
        let mut unrequested_part_paths = disk_parts
            .iter()
            .filter(|(name, _)| !expected_parts.contains(*name))
            .map(|(_, path)| path.clone())
            .collect::<Vec<_>>();
        unrequested_part_paths.sort_by(|a, b| compare_objection_parts(a, b, &base));
        let mut cleanup_paths = part_paths.clone();
        let mut input_paths = part_paths.clone();
        for entry in fs::read_dir(&folder)? {
            let path = entry?.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if matches!(classify_objection_pdf(name), Some((ref file_base, false)) if file_base == &base)
            {
                input_paths.push(path.clone());
                if path != output_path {
                    cleanup_paths.push(path);
                }
            }
        }
        input_paths.sort_by(|a, b| compare_objection_parts(a, b, &base));
        cleanup_paths.sort_by(|a, b| compare_objection_parts(a, b, &base));
        let mut total_pages = 0u32;
        let mut source_snapshots = Vec::with_capacity(input_paths.len());
        for path in &input_paths {
            let resolved = fs::canonicalize(path).map_err(|err| {
                format!(
                    "no se pudo resolver el PDF de origen {}: {}",
                    path.display(),
                    err
                )
            })?;
            if !resolved.starts_with(allowed_root) {
                return Err(format!(
                    "el PDF de origen escapa de HE1_OBJECTION_DATA_ROOT: {}",
                    path.display()
                )
                .into());
            }
            let pages = page_count(path)?;
            total_pages = total_pages
                .checked_add(pages)
                .ok_or("demasiadas páginas en el grupo")?;
            source_snapshots.push(snapshot_objection_source(path, pages)?);
        }
        let mut unrequested_snapshots = Vec::with_capacity(unrequested_part_paths.len());
        for path in &unrequested_part_paths {
            unrequested_snapshots.push(snapshot_objection_source(path, page_count(path)?)?);
        }

        let mut db_groups = Vec::new();
        for (id_descarga, names) in by_download {
            let mut part_names = names.into_iter().collect::<Vec<_>>();
            part_names.sort_by(|a, b| compare_objection_names(a, b, &base));
            db_groups.push(ObjectionDbChange {
                id_descarga,
                tramite: tramite.clone(),
                base: base.clone(),
                canonical_name: format!("{}.pdf", base),
                part_names,
                output_path: output_path.display().to_string(),
                total_pages,
            });
        }
        plans.push(ObjectionMergePlan {
            tramite,
            folder,
            base,
            part_paths,
            unrequested_part_paths,
            cleanup_paths,
            input_paths,
            source_snapshots,
            unrequested_snapshots,
            output_path,
            total_pages,
            db_groups,
        });
    }
    Ok(plans)
}

fn snapshot_objection_source(
    path: &Path,
    pages: u32,
) -> Result<ObjectionSourceSnapshot, Box<dyn std::error::Error>> {
    let metadata = fs::metadata(path)?;
    let modified_unix_seconds = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    Ok(ObjectionSourceSnapshot {
        filename: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("desconocido")
            .to_string(),
        path: path.display().to_string(),
        size_bytes: metadata.len(),
        pages,
        modified_unix_seconds,
        sha256: sha256_file(path)?,
    })
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn build_objection_execution_plan(
    label: &str,
) -> Result<ObjectionExecutionPlan, Box<dyn std::error::Error>> {
    let helper = objection_db_helper_path()?;
    let db_plan = run_objection_db_plan(&helper, label)?;
    if db_plan.label != label {
        return Err("la etiqueta devuelta por el auxiliar Oracle no coincide".into());
    }
    let data_root = objection_data_root()?;
    let plans = build_objection_merge_plans(&db_plan, &data_root)?;
    let report = ObjectionPreviewReport {
        label: label.to_string(),
        groups: plans
            .iter()
            .map(|plan| ObjectionPreviewGroup {
                tramite: plan.tramite.clone(),
                document: plan.base.clone(),
                destination: plan.output_path.display().to_string(),
                total_pages: plan.total_pages,
                oracle_items: plan
                    .db_groups
                    .iter()
                    .map(|group| ObjectionPreviewOracleItem {
                        id_descarga: group.id_descarga.clone(),
                        source_names: group.part_names.clone(),
                    })
                    .collect(),
                source_files: plan.source_snapshots.clone(),
                unrequested_files: plan.unrequested_snapshots.clone(),
            })
            .collect(),
    };
    let fingerprint = sha256_bytes(&serde_json::to_vec(&report)?);
    Ok(ObjectionExecutionPlan {
        helper,
        plans,
        report,
        fingerprint,
    })
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn prepare_telegram_objection_preview(
    label: &str,
) -> Result<TelegramObjectionPreview, Box<dyn std::error::Error>> {
    let execution = build_objection_execution_plan(label)?;
    if execution.plans.is_empty() {
        return Err(format!("la etiqueta {} no tiene grupos fraccionados", label).into());
    }
    write_telegram_objection_preview(label, &execution.report, &execution.fingerprint)
}

fn write_telegram_objection_preview(
    label: &str,
    report: &ObjectionPreviewReport,
    fingerprint: &str,
) -> Result<TelegramObjectionPreview, Box<dyn std::error::Error>> {
    let preview_root = env::current_dir()?
        .join("he1_respaldo")
        .join("objeciones")
        .join("vistas_previas");
    fs::create_dir_all(&preview_root)?;
    let report_path = preview_root.join(format!(
        "{}_antes_{}.csv",
        objection_operation_id(label),
        &fingerprint[..12.min(fingerprint.len())]
    ));
    write_objection_preview_csv(report, &report_path)?;
    Ok(TelegramObjectionPreview {
        label: label.to_string(),
        fingerprint: fingerprint.to_string(),
        report_path,
        groups: report.groups.len(),
        pages: report
            .groups
            .iter()
            .map(|group| u64::from(group.total_pages))
            .sum(),
    })
}

fn write_objection_preview_csv(
    report: &ObjectionPreviewReport,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut contents = String::from(
        "estado,etiqueta,trámite,documento,archivo_origen,ruta_origen,tamaño_bytes,páginas,modificado_unix,sha256,destino,páginas_destino_plan,ítems_oracle\n",
    );
    for group in &report.groups {
        let oracle_items = group
            .oracle_items
            .iter()
            .map(|item| format!("{}:{}", item.id_descarga, item.source_names.join("|")))
            .collect::<Vec<_>>()
            .join("; ");
        for (state, sources) in [
            ("se_uniria", &group.source_files),
            ("fuera_de_etiqueta_se_conserva", &group.unrequested_files),
        ] {
            for source in sources {
                let fields = [
                    state.to_string(),
                    report.label.clone(),
                    group.tramite.clone(),
                    group.document.clone(),
                    source.filename.clone(),
                    source.path.clone(),
                    source.size_bytes.to_string(),
                    source.pages.to_string(),
                    source
                        .modified_unix_seconds
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    source.sha256.clone(),
                    group.destination.clone(),
                    group.total_pages.to_string(),
                    oracle_items.clone(),
                ];
                contents.push_str(
                    &fields
                        .iter()
                        .map(|field| csv_escape(field))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                contents.push('\n');
            }
        }
    }
    let parent = path.parent().ok_or("ruta de reporte previa inválida")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("csv.tmp");
    fs::write(&temporary, contents.as_bytes())?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn csv_escape(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) fn confirm_telegram_objection_plan(
    label: &str,
    expected_fingerprint: &str,
) -> Result<TelegramObjectionApply, Box<dyn std::error::Error>> {
    match run_objection_mode_checked(label, false, Some(expected_fingerprint))? {
        ObjectionRunOutcome::Applied(result) => Ok(TelegramObjectionApply::Applied {
            summary: result.summary,
            manifest_path: result.manifest_path,
            report_path: result.report_path,
        }),
        ObjectionRunOutcome::PlanChanged(preview) => {
            Ok(TelegramObjectionApply::PlanChanged(preview))
        }
        ObjectionRunOutcome::NoGroups => {
            Err(format!("ya no hay grupos fraccionados para la etiqueta {}", label).into())
        }
        ObjectionRunOutcome::DryRun => Err("estado inesperado al confirmar objeciones".into()),
    }
}

fn classify_objection_pdf(name: &str) -> Option<(String, bool)> {
    let path = Path::new(name);
    if !path.extension()?.to_str()?.eq_ignore_ascii_case("pdf") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let normalized = normalize_stem(stem).to_ascii_uppercase();
    if normalized == "RDH" {
        return Some(("RHD".to_string(), !stem.eq_ignore_ascii_case("RDH")));
    }
    if let Some(suffix) = normalized.strip_prefix("RDH") {
        if is_allowed_variant_suffix(suffix) {
            return Some(("RHD".to_string(), true));
        }
    }
    let base = detect_group_base(stem)?;
    if normalized == base.to_ascii_uppercase() {
        return Some((base.to_string(), !stem.eq_ignore_ascii_case(base)));
    }
    let suffix = normalized.strip_prefix(&base.to_ascii_uppercase())?;
    if is_allowed_variant_suffix(suffix) {
        Some((base.to_string(), true))
    } else {
        None
    }
}

fn compare_objection_parts(a: &Path, b: &Path, base: &str) -> std::cmp::Ordering {
    objection_sort_key(a.file_name().and_then(|v| v.to_str()).unwrap_or(""), base).cmp(
        &objection_sort_key(b.file_name().and_then(|v| v.to_str()).unwrap_or(""), base),
    )
}

fn compare_objection_names(a: &str, b: &str, base: &str) -> std::cmp::Ordering {
    objection_sort_key(a, base).cmp(&objection_sort_key(b, base))
}

fn objection_sort_key(name: &str, base: &str) -> (u8, u8, u64, String) {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or(name);
    let mut normalized = normalize_stem(stem).to_ascii_uppercase();
    let base_upper = base.to_ascii_uppercase();
    if base_upper == "RHD" {
        if let Some(suffix) = normalized.strip_prefix("RDH") {
            normalized = format!("RHD{}", suffix);
        }
    }
    if normalized == base_upper {
        return (0, 0, 0, normalized);
    }
    let suffix = normalized.strip_prefix(&base_upper).unwrap_or("");
    let suffix = suffix.trim_start_matches(|c| c == '_' || c == '-');
    if let Ok(number) = suffix.parse::<u64>() {
        (1, 0, number, normalized)
    } else {
        (1, 1, u64::MAX, normalized)
    }
}

fn objection_refresh_template() -> Result<String, Box<dyn std::error::Error>> {
    let template = env::var("HE1_OBJECTION_REFRESH_URL_TEMPLATE")
        .unwrap_or_else(|_| "http://127.0.0.1:8091/api/cache/tramite/{tramite}".to_string());
    if !(template.starts_with("http://") || template.starts_with("https://"))
        || !template.contains("{tramite}")
    {
        return Err(
            "HE1_OBJECTION_REFRESH_URL_TEMPLATE debe ser una URL HTTP(S) e incluir {tramite}"
                .into(),
        );
    }
    Ok(template)
}

fn refresh_objection_cache(
    client: &reqwest::blocking::Client,
    template: &str,
    tramite: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = template.replace("{tramite}", &encode_url_segment(tramite));
    let mut last_error = None;
    for attempt in 0..3 {
        match client
            .post(&url)
            .send()
            .and_then(|response| response.error_for_status())
        {
            Ok(_) => return Ok(()),
            Err(err) => last_error = Some(err),
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    match last_error {
        Some(err) => Err(err.into()),
        None => Err("falló la actualización de caché".into()),
    }
}

fn encode_url_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

fn pending_refresh_path(root: &Path) -> PathBuf {
    root.join("he1_respaldo")
        .join("objeciones")
        .join("pending_refresh.json")
}

fn append_pending_refreshes(
    root: &Path,
    additions: &[PendingObjectionRefresh],
) -> Result<(), Box<dyn std::error::Error>> {
    let path = pending_refresh_path(root);
    let mut pending = if path.exists() {
        serde_json::from_slice::<Vec<PendingObjectionRefresh>>(&fs::read(&path)?)?
    } else {
        Vec::new()
    };
    for item in additions {
        if !pending
            .iter()
            .any(|old| old.label == item.label && old.tramite == item.tramite)
        {
            pending.push(item.clone());
        }
    }
    write_json_atomic(&path, &pending)
}

fn remove_pending_refresh(
    root: &Path,
    label: &str,
    tramite: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = pending_refresh_path(root);
    if !path.exists() {
        return Ok(());
    }
    let mut pending = serde_json::from_slice::<Vec<PendingObjectionRefresh>>(&fs::read(&path)?)?;
    pending.retain(|item| item.label != label || item.tramite != tramite);
    if pending.is_empty() {
        fs::remove_file(path)?;
    } else {
        write_json_atomic(&path, &pending)?;
    }
    Ok(())
}

fn retry_pending_objection_refreshes() -> Result<(), Box<dyn std::error::Error>> {
    let template = objection_refresh_template()?;
    let root = env::current_dir()?;
    let path = pending_refresh_path(&root);
    if !path.exists() {
        println!("No hay refrescos de caché pendientes.");
        return Ok(());
    }
    let pending = serde_json::from_slice::<Vec<PendingObjectionRefresh>>(&fs::read(&path)?)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut remaining = Vec::new();
    for item in &pending {
        match refresh_objection_cache(&client, &template, &item.tramite) {
            Ok(()) => println!(
                "Caché actualizada para trámite {} (objeción {}).",
                item.tramite, item.label
            ),
            Err(err) => {
                eprintln!("Sigue pendiente el trámite {}: {}", item.tramite, err);
                remaining.push(item.clone());
            }
        }
    }
    if remaining.is_empty() {
        fs::remove_file(path)?;
    } else {
        write_json_atomic(&path, &remaining)?;
    }
    Ok(())
}

fn objection_operation_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}_{}_{}", label, nanos, std::process::id())
}

fn rollback_objection_outputs(
    mutations: &[ObjectionFileMutation],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut errors = Vec::new();
    for mutation in mutations.iter().rev() {
        if let Some(backup) = &mutation.previous_output_backup {
            let restore_tmp = mutation.output_path.with_extension("restore.tmp.pdf");
            if let Err(err) = fs::copy(backup, &restore_tmp)
                .and_then(|_| fs::rename(&restore_tmp, &mutation.output_path))
            {
                errors.push(format!("{}: {}", mutation.output_path.display(), err));
            }
        } else if mutation.output_path.exists() {
            if let Err(err) = fs::remove_file(&mutation.output_path) {
                errors.push(format!("{}: {}", mutation.output_path.display(), err));
            }
        }
        let _ = fs::remove_file(&mutation.temporary_path);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; ").into())
    }
}

fn write_json_atomic(
    path: &Path,
    value: &impl Serialize,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("ruta de manifiesto inválida")?;
    fs::create_dir_all(parent)?;
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn run_directory_list(
    list_file: &Path,
    label: Option<&str>,
    progress: Option<&dyn ProgressSink>,
) -> Result<(), Box<dyn std::error::Error>> {
    let DirectoryListReadResult {
        directories,
        errors,
        stats,
    } = read_directory_list(list_file)?;
    if !errors.is_empty() {
        println!("errores en el archivo de directorios:");
        for error in &errors {
            println!("  linea {}: {}", error.line_number, error.message);
        }
        println!(
            "se omitieron {} linea(s) con error; quedan registradas en el log",
            errors.len()
        );
        emit_progress(
            progress,
            format!(
                "archivo de directorios con {} linea(s) omitida(s): {}",
                errors.len(),
                list_file.display()
            ),
        );
    }

    run_directories(
        directories,
        list_file,
        label,
        &errors,
        Some(&stats),
        progress,
    )
}

fn run_directories(
    directories: Vec<PathBuf>,
    source_label: &Path,
    label: Option<&str>,
    directory_errors: &[DirectoryListError],
    directory_stats: Option<&DirectoryListStats>,
    progress: Option<&dyn ProgressSink>,
) -> Result<(), Box<dyn std::error::Error>> {
    let log_root = log_root_for(source_label);
    let log_path = log_root.join("Cambios.txt");
    println!("log registrado en: {}", log_path.display());
    let mut change_log = ChangeLog::new(&log_root)?;
    let mut backup_session =
        RunBackupSession::new(source_label, &log_root, label, directory_stats)?;
    let run_label = label.unwrap_or("sin_etiqueta");

    change_log.write_line(format!("INICIO corrida: {}", timestamp_now()))?;
    change_log.write_line(format!("FUENTE: {}", source_label.display()))?;
    if let Some(label) = label {
        change_log.write_line(format!("ETIQUETA: {}", label))?;
    }
    if let Some(stats) = directory_stats {
        change_log.write_line(format!(
            "TXT: lineas_totales={} registros_utiles={} carpetas_validas={} duplicados_omitidos={} errores={}",
            stats.total_lines,
            stats.useful_lines,
            stats.valid_directories,
            stats.duplicate_directories,
            stats.errors
        ))?;
    }
    change_log.write_line(format!("RESPALDO: {}", backup_session.root.display()))?;
    emit_progress(
        progress,
        format!(
            "inicio corrida {} | fuente {} | carpetas {} | respaldo {}",
            run_label,
            source_label.display(),
            directories.len(),
            backup_session.root.display()
        ),
    );
    if !directory_errors.is_empty() {
        change_log.write_line(format!(
            "ARCHIVO DE DIRECTORIOS CON ERRORES: {}",
            source_label.display()
        ))?;
        for error in directory_errors {
            change_log.write_line(format!(
                "OMITIDA linea {}: {}",
                error.line_number, error.message
            ))?;
        }
        emit_progress(
            progress,
            format!(
                "archivo de directorios con {} error(es) omitidos",
                directory_errors.len()
            ),
        );
    }
    change_log.write_line("FASE 1: unificacion por reglas canonicas")?;

    let mut run_summary = RunSummary {
        attempted_folders: directories.len(),
        ..RunSummary::default()
    };

    let total_folders = directories.len();
    if let Some(progress) = progress {
        progress.update_folder_progress(0, total_folders);
    }
    for (index, folder) in directories.into_iter().enumerate() {
        let folder_number = index + 1;
        if let Some(progress) = progress {
            progress.update_folder_progress(folder_number, total_folders);
        }
        emit_progress(
            progress,
            format!(
                "carpeta {}/{}: {}",
                folder_number,
                total_folders,
                folder.display()
            ),
        );
        match process_folder(&folder, &mut change_log, &mut backup_session, progress) {
            Ok(summary) => {
                run_summary.merged_groups += summary.merged_groups;
                run_summary.merged_files += summary.merged_files;
                run_summary.renamed_files += summary.renamed_files;
                run_summary.processed_folders += 1;
                emit_progress(
                    progress,
                    format!(
                        "carpeta {}/{} finalizada: {} grupo(s) consolidados, {} archivo(s) renombrado(s)",
                        folder_number, total_folders, summary.merged_groups, summary.renamed_files
                    ),
                );
            }
            Err(err) => {
                println!("error en carpeta {}: {}", folder.display(), err);
                change_log.write_line(format!("ERROR carpeta {}: {}", folder.display(), err))?;
                emit_progress(
                    progress,
                    format!("error en carpeta {}: {}", folder.display(), err),
                );
            }
        }
    }

    println!(
        "resumen: {} carpetas procesadas, {} grupos consolidados, {} PDFs candidatos, {} renombrados directos",
        run_summary.processed_folders,
        run_summary.merged_groups,
        run_summary.merged_files,
        run_summary.renamed_files
    );
    change_log.write_line(format!(
        "RESUMEN: {} carpetas procesadas, {} grupos consolidados, {} PDFs candidatos, {} renombrados directos",
        run_summary.processed_folders,
        run_summary.merged_groups,
        run_summary.merged_files,
        run_summary.renamed_files
    ))?;
    change_log.write_line(format!("FIN corrida: {}", timestamp_now()))?;
    backup_session.finish_with_summary(&run_summary)?;

    Ok(())
}

fn process_folder(
    folder: &Path,
    change_log: &mut ChangeLog,
    backup_session: &mut RunBackupSession,
    progress: Option<&dyn ProgressSink>,
) -> Result<FolderSummary, Box<dyn std::error::Error>> {
    if !folder.exists() {
        return Err(format!("la carpeta no existe: {}", folder.display()).into());
    }
    if !folder.is_dir() {
        return Err(format!("la ruta no es carpeta: {}", folder.display()).into());
    }

    let processed_marker = folder.join(".he1_procesado");
    if processed_marker.exists() {
        println!("saltando carpeta ya procesada: {}", folder.display());
        change_log.write_line(format!(
            "CARPETA OMITIDA (ya procesada): {}",
            folder.display()
        ))?;
        emit_progress(
            progress,
            format!("carpeta omitida (ya procesada): {}", folder.display()),
        );
        return Ok(FolderSummary::default());
    }

    println!("analizando carpeta: {}", folder.display());
    change_log.write_line(format!("CARPETA: {}", folder.display()))?;
    backup_session.record_folder(folder)?;
    let folder_work_root = backup_session.work_dir_for(folder);
    let mut folder_audit = FolderAudit::new(&folder_work_root)?;
    folder_audit.write_line(format!("AUDITORIA: {}", folder.display()))?;
    folder_audit.write_line(format!("INICIO: {}", timestamp_now()))?;
    folder_audit.write_line(format!("RUTA_TRABAJO: {}", folder_work_root.display()))?;

    let mut pdf_files = Vec::new();
    let mut pdf_count = 0usize;
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("pdf"))
            != Some(true)
        {
            continue;
        }

        pdf_count += 1;
        pdf_files.push(path);
    }

    let mut ignored_files = Vec::new();
    for path in &pdf_files {
        let file_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) => stem,
            None => continue,
        };
        if let Some(base) = detect_group_base(stem) {
            if base != stem {
                ignored_files.push((file_name.to_string(), base.to_string()));
            }
        } else {
            println!("ignorado: {}", file_name);
            change_log.write_line(format!("  IGNORADO: {}", file_name))?;
            folder_audit.write_line(format!("  IGNORADO: {}", file_name))?;
        }
    }

    println!("  PDFs encontrados: {}", pdf_count);
    change_log.write_line(format!("  PDFs encontrados: {}", pdf_count))?;
    folder_audit.write_line(format!("  PDFs encontrados: {}", pdf_count))?;
    emit_progress(
        progress,
        format!(
            "carpeta {}: {} PDF(s) detectados",
            folder.display(),
            pdf_count
        ),
    );

    for (file_name, base) in ignored_files {
        change_log.write_line(format!("  OBSERVADO: {} | base {}", file_name, base))?;
        folder_audit.write_line(format!("  OBSERVADO: {} | base {}", file_name, base))?;
    }

    let mut summary = FolderSummary::default();
    let mut folder_backups = HashSet::new();
    let mut folder_outputs = HashSet::new();

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        for base in VALID_NAMES {
            let mut files = collect_base_files(&pdf_files, base);
            if files.is_empty() {
                continue;
            }
            files.sort();
            println!("  base {} con {} archivos", base, files.len());
            change_log.write_line(format!("  base {} con {} archivos", base, files.len()))?;
            folder_audit.write_line(format!("  base {} con {} archivos", base, files.len()))?;
            change_log.write_line(format!("    antes: {}", describe_files(&files)))?;
            folder_audit.write_line(format!("    antes: {}", describe_files(&files)))?;
            emit_progress(
                progress,
                format!(
                    "carpeta {}: base {} con {} archivo(s)",
                    folder.display(),
                    base,
                    files.len()
                ),
            );

            let output = canonical_output_path(folder, base);
            let (files, case_only_duplicates) = split_case_variant_duplicates(files, &output);
            if !case_only_duplicates.is_empty() {
                change_log.write_line(format!(
                    "    duplicados de casing detectados: {}",
                    describe_files(&case_only_duplicates)
                ))?;
                folder_audit.write_line(format!(
                    "    duplicados de casing detectados: {}",
                    describe_files(&case_only_duplicates)
                ))?;
            }
            let mut managed_files = files.clone();
            managed_files.extend(case_only_duplicates.iter().cloned());

            if files.len() == 1 {
                let source = &files[0];
                if paths_equivalent(source, &output) {
                    if !case_only_duplicates.is_empty() {
                        for path in &case_only_duplicates {
                            backup_path_if_needed(backup_session, &mut folder_backups, path)?;
                        }
                        delete_sources(&managed_files, &output)?;
                        println!(
                            "  base {} conservada en {} y duplicados de casing eliminados",
                            base,
                            output.display()
                        );
                        change_log.write_line(format!("    despues: {}", output.display()))?;
                        change_log.write_line(
                            "    accion: conservado archivo canonico y eliminados duplicados de casing",
                        )?;
                        folder_audit.write_line(format!("    despues: {}", output.display()))?;
                        folder_audit.write_line(
                            "    accion: conservado archivo canonico y eliminados duplicados de casing",
                        )?;
                        emit_progress(
                            progress,
                            format!(
                                "carpeta {}: base {} conservada en {} y duplicados de casing eliminados",
                                folder.display(),
                                base,
                                output.display()
                            ),
                        );
                    }
                    println!("  base {} ya consolidada en {}", base, output.display());
                    change_log.write_line(format!("    despues: {}", output.display()))?;
                    folder_audit.write_line(format!("    despues: {}", output.display()))?;
                    change_log.write_line(
                        "    accion: omitido para no reprocesar el archivo unificado",
                    )?;
                    folder_audit.write_line(
                        "    accion: omitido para no reprocesar el archivo unificado",
                    )?;
                    emit_progress(
                        progress,
                        format!(
                            "carpeta {}: base {} ya consolidada en {}",
                            folder.display(),
                            base,
                            output.display()
                        ),
                    );
                    continue;
                }

                backup_session.record_output(&output)?;
                folder_outputs.insert(output.clone());
                for path in &managed_files {
                    backup_path_if_needed(backup_session, &mut folder_backups, path)?;
                }
                if output.exists() {
                    backup_path_if_needed(backup_session, &mut folder_backups, &output)?;
                    fs::remove_file(&output)?;
                }
                fs::rename(source, &output)?;
                println!("  base {} renombrada directo a {}", base, output.display());
                change_log.write_line(format!("    despues: {}", output.display()))?;
                change_log.write_line("    accion: renombrado directo sin union")?;
                folder_audit.write_line(format!("    despues: {}", output.display()))?;
                folder_audit.write_line("    accion: renombrado directo sin union")?;
                summary.renamed_files += 1;
                emit_progress(
                    progress,
                    format!(
                        "carpeta {}: base {} renombrada a {}",
                        folder.display(),
                        base,
                        output.display()
                    ),
                );
                continue;
            }

            summary.merged_groups += 1;
            summary.merged_files += files.len();
            let mut source_pages = 0u32;
            let mut source_size = 0u64;
            let mut per_file_pages = Vec::with_capacity(files.len());
            for path in &files {
                let pages = page_count(path)?;
                let size = fs::metadata(path)?.len();
                source_pages += pages;
                source_size += size;
                per_file_pages.push((path.clone(), pages, size));
            }

            backup_session.record_output(&output)?;
            folder_outputs.insert(output.clone());
            for path in &managed_files {
                backup_path_if_needed(backup_session, &mut folder_backups, path)?;
            }
            if output.exists() && !files.iter().any(|p| paths_equivalent(p, &output)) {
                backup_path_if_needed(backup_session, &mut folder_backups, &output)?;
            }

            let aux_dir = folder_work_root.join(base);
            fs::create_dir_all(&aux_dir)?;
            let aux_paths = build_aux_paths(&files, &aux_dir)?;
            copy_to_aux(&files, &aux_paths)?;
            folder_audit.write_line(format!("    aux_dir: {}", aux_dir.display()))?;
            folder_audit.write_line(format!("    copias_aux: {}", describe_files(&aux_paths)))?;
            let temp_output = folder.join(format!("{}.tmp.pdf", base));
            merge_pdfs(&aux_paths, &temp_output)?;
            if output.exists() {
                fs::remove_file(&output)?;
            }
            fs::rename(&temp_output, &output)?;

            let merged_pages = page_count(&output)?;
            let merged_size = fs::metadata(&output)?.len();
            if merged_pages != source_pages {
                change_log.write_line(format!(
                    "    ERROR verificacion fallo: paginas fuente={} paginas salida={}",
                    source_pages, merged_pages
                ))?;
                folder_audit.write_line(format!(
                    "    ERROR verificacion fallo: paginas fuente={} paginas salida={}",
                    source_pages, merged_pages
                ))?;
                emit_progress(
                    progress,
                    format!(
                        "carpeta {}: base {} con error de verificacion de paginas",
                        folder.display(),
                        base
                    ),
                );
                return Err(format!(
                    "verificacion fallo para {}: paginas fuente={} paginas salida={}",
                    output.display(),
                    source_pages,
                    merged_pages
                )
                .into());
            }
            if merged_size == 0 {
                change_log.write_line("    ERROR verificacion fallo: bytes salida=0")?;
                folder_audit.write_line("    ERROR verificacion fallo: bytes salida=0")?;
                emit_progress(
                    progress,
                    format!(
                        "carpeta {}: base {} con error de verificacion de bytes",
                        folder.display(),
                        base
                    ),
                );
                return Err(format!(
                    "verificacion fallo para {}: bytes salida=0",
                    output.display()
                )
                .into());
            }

            delete_sources(&managed_files, &output)?;
            cleanup_aux(&aux_paths)?;
            if aux_dir.exists() && aux_dir.read_dir()?.next().is_none() {
                fs::remove_dir_all(&aux_dir)?;
            }

            println!(
                "{} -> base {} | {} archivos | {} paginas fuente | {} bytes fuente | {} paginas salida | {} bytes salida",
                folder.display(),
                base,
                files.len(),
                source_pages,
                source_size,
                merged_pages,
                merged_size
            );
            change_log.write_line(format!("    salida: {}", output.display()))?;
            folder_audit.write_line(format!("    salida: {}", output.display()))?;
            for (path, pages, size) in per_file_pages {
                change_log.write_line(format!(
                    "    fuente verificada: {} | paginas={} | bytes={}",
                    path.display(),
                    pages,
                    size
                ))?;
                folder_audit.write_line(format!(
                    "    fuente verificada: {} | paginas={} | bytes={}",
                    path.display(),
                    pages,
                    size
                ))?;
            }
            change_log.write_line(format!(
                "    verificacion: paginas fuente={} bytes fuente={} paginas salida={} bytes salida={}",
                source_pages, source_size, merged_pages, merged_size
            ))?;
            folder_audit.write_line(format!(
                "    verificacion: paginas fuente={} bytes fuente={} paginas salida={} bytes salida={}",
                source_pages, source_size, merged_pages, merged_size
            ))?;
            change_log.write_line("    estado: completado con exito")?;
            folder_audit.write_line("    estado: completado con exito")?;
            emit_progress(
                progress,
                format!(
                    "carpeta {}: base {} consolidada en {}",
                    folder.display(),
                    base,
                    output.display()
                ),
            );
        }

        fs::write(
            &processed_marker,
            format!("procesado_en={}\n", timestamp_now()),
        )?;
        change_log.write_line(format!("MARCA CREADA: {}", processed_marker.display()))?;
        folder_audit.write_line(format!("MARCA CREADA: {}", processed_marker.display()))?;
        emit_progress(
            progress,
            format!(
                "carpeta finalizada: {} | grupos={} | archivos={}",
                folder.display(),
                summary.merged_groups,
                summary.merged_files
            ),
        );
        Ok(())
    })();

    match result {
        Ok(()) => Ok(summary),
        Err(err) => {
            restore_folder_state(folder, &folder_backups, &folder_outputs, backup_session)?;
            Err(err)
        }
    }
}

fn backup_path_if_needed(
    backup_session: &mut RunBackupSession,
    folder_backups: &mut HashSet<PathBuf>,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if folder_backups.insert(path.to_path_buf()) {
        backup_session.backup_file(path)?;
    }
    Ok(())
}

fn restore_folder_state(
    folder: &Path,
    backed_up: &HashSet<PathBuf>,
    outputs: &HashSet<PathBuf>,
    backup_session: &RunBackupSession,
) -> Result<(), Box<dyn std::error::Error>> {
    for output in outputs {
        if !backed_up.iter().any(|path| paths_equivalent(path, output)) && output.exists() {
            fs::remove_file(output)?;
        }
    }

    for original in backed_up {
        let backup = backup_session.backup_path_for(original);
        if !backup.exists() {
            return Err(format!(
                "no se encontro respaldo para restaurar {}",
                original.display()
            )
            .into());
        }
        if let Some(parent) = original.parent() {
            fs::create_dir_all(parent)?;
        }
        if original.exists() {
            fs::remove_file(original)?;
        }
        fs::copy(&backup, original)?;
    }

    cleanup_generated_artifacts(folder, &backup_session.work_dir_for(folder))?;
    Ok(())
}

fn cleanup_generated_artifacts(
    folder: &Path,
    work_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let processed_marker = folder.join(".he1_procesado");
    if processed_marker.exists() {
        fs::remove_file(processed_marker)?;
    }

    if work_dir.exists() {
        for entry in fs::read_dir(work_dir)? {
            let entry = entry?;
            let path = entry.path();
            let keep_audit = path.file_name().and_then(|s| s.to_str()) == Some("auditoria.txt");
            if keep_audit {
                continue;
            }

            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else if path.is_file() {
                fs::remove_file(path)?;
            }
        }
    }

    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        let is_tmp_pdf = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|name| name.ends_with(".tmp.pdf"))
            .unwrap_or(false);
        if path.is_file() && is_tmp_pdf {
            fs::remove_file(path)?;
        }
    }

    Ok(())
}

fn print_usage() {
    println!("uso:");
    println!("  he1-unificar-pdfs --label <etiqueta> <ruta.txt | carpeta>");
    println!("  he1-unificar-pdfs --restore <etiqueta | ruta_respaldo_o_manifest.txt>");
    println!("  he1-unificar-pdfs --report <etiqueta | ruta_manifest_o_respaldo>");
    println!("  he1-unificar-pdfs --check <etiqueta | ruta_manifest_o_respaldo> <carpeta>");
    println!("  he1-unificar-pdfs --convert-paths <entrada.txt> <salida.txt>");
    println!("  he1-unificar-pdfs --modo objeciones --label <etiqueta> [--dry-run]");
    println!("  he1-unificar-pdfs --modo objeciones --reintentar-refresh");
    println!("  he1-unificar-pdfs --telegram");
    println!();
    println!("ejemplo:");
    println!(
        "  he1-unificar-pdfs --label folder_0001 G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\pdfs\\folder_0001"
    );
    println!("  he1-unificar-pdfs --restore folder_0001");
    println!("  he1-unificar-pdfs --report folder_0001");
    println!(
        "  he1-unificar-pdfs --check folder_0001 G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\pdfs\\folder_0001"
    );
    println!(
        "  he1-unificar-pdfs --convert-paths G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\fuentes_txt\\PATH_DIRECTORIOS.txt G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\fuentes_txt\\PATH_DIRECTORIOS.linux.txt"
    );
    println!(
        "  he1-unificar-pdfs --restore G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\he1_respaldo\\run_...\\manifest.txt"
    );
    println!(
        "  he1-unificar-pdfs --report G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\he1_respaldo\\run_...\\manifest.txt"
    );
    println!(
        "  he1-unificar-pdfs --check G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\he1_respaldo\\run_...\\manifest.txt G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\pdfs\\folder_0001"
    );
    println!("  he1-unificar-pdfs --telegram");
    println!();
    println!("comportamiento:");
    println!("  - si la entrada es .txt, cada linea valida se interpreta como carpeta");
    println!("  - si la entrada es carpeta, se procesa esa carpeta como unidad");
    println!("  - --convert-paths crea una copia nueva con rutas Windows traducidas a Linux");
    println!("  - solo se procesan archivos PDF que cumplan la regla canonica");
    println!("  - los originales que se tocan se respaldan para poder restaurar");
    println!("  - la salida final queda en la misma carpeta original");
    println!("  - el respaldo se guarda en una carpeta visible he1_respaldo");
    println!("  - --restore reconstruye los originales y elimina los PDFs generados");
    println!("  - --report genera un HTML con la fecha de proceso en hora de Ecuador");
    println!("  - --check verifica si una carpeta figura en el manifest y si conserva su marcador");
    println!("  - --telegram escucha comandos por Telegram desde este mismo equipo");
    println!("  - deja un log Cambios.txt con el detalle de la ejecucion");
    println!("  - documentacion de reglas: ver REGLA_UNIFICACION.md");
    println!();
    println!("telegram:");
    println!("  - requiere TELEGRAM_BOT_TOKEN");
    println!("  - requiere TELEGRAM_CHAT_ID");
    println!("  - requiere HE1_INPUT para /process y /process_report");
    println!(
        "  - comandos: /process <etiqueta>, /process_report <etiqueta>, /restore <etiqueta>, /report <etiqueta>"
    );
    println!();
    println!("regla de nombres:");
    println!("  - acepta base.pdf");
    println!("  - acepta variantes base_*.pdf o base-*.pdf");
    println!(
        "  - tambien acepta nombres que se reducen al canonico por espacios, parentesis, corchetes, llaves o puntos"
    );
    println!(
        "  - la comparacion no distingue mayusculas/minusculas; RHd-copia_extra.PDF puede entrar como RHD.pdf"
    );
    println!("  - no acepta nombres pegados sin _ o - como PI13.pdf o AES4545.pdf");
}

fn timestamp_now() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => format!("{}", dur.as_secs()),
        Err(_) => "0".to_string(),
    }
}

const ECUADOR_OFFSET_SECONDS: i64 = -5 * 3600;

pub fn auto_process_label() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => format_unix_compact_with_offset(dur.as_secs() as i64, ECUADOR_OFFSET_SECONDS),
        Err(_) => "000000000000".to_string(),
    }
}

fn timestamp_to_ecuador(timestamp: &str) -> Option<String> {
    let unix_seconds = timestamp.parse::<i64>().ok()?;
    Some(format_unix_seconds_with_offset(
        unix_seconds,
        ECUADOR_OFFSET_SECONDS,
    ))
}

fn system_time_to_unix_timestamp(time: SystemTime) -> Option<String> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_secs().to_string())
}

fn file_modified_timestamp(path: &Path) -> Option<String> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_to_unix_timestamp)
}

fn parse_unix_timestamp(value: &str) -> Option<i64> {
    value.parse::<i64>().ok()
}

fn format_duration_seconds(total_seconds: i64) -> String {
    let total_seconds = total_seconds.max(0);
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{}h {:02}m {:02}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {:02}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

fn format_unix_seconds_with_offset(unix_seconds: i64, offset_seconds: i64) -> String {
    let local_seconds = unix_seconds + offset_seconds;
    let days = local_seconds.div_euclid(86_400);
    let seconds_of_day = local_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hour, minute, second
    )
}

fn format_unix_compact_with_offset(unix_seconds: i64, offset_seconds: i64) -> String {
    let local_seconds = unix_seconds + offset_seconds;
    let days = local_seconds.div_euclid(86_400);
    let seconds_of_day = local_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;

    format!("{:04}{:02}{:02}{:02}{:02}", year, month, day, hour, minute)
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };

    (year, month as u32, day as u32)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn canonical_output_path(folder: &Path, base: &str) -> PathBuf {
    folder.join(format!("{}.pdf", base))
}

fn log_root_for(source_label: &Path) -> PathBuf {
    match source_label.parent() {
        Some(parent) if parent.as_os_str().is_empty() => PathBuf::from("."),
        Some(parent) => match parent.file_name().and_then(|name| name.to_str()) {
            Some("fuentes_txt") | Some("pdfs") => parent.parent().unwrap_or(parent).to_path_buf(),
            _ => parent.to_path_buf(),
        },
        None => PathBuf::from("."),
    }
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut left_components = left.components();
        let mut right_components = right.components();

        loop {
            match (left_components.next(), right_components.next()) {
                (None, None) => return true,
                (Some(l), Some(r)) if components_equivalent(l, r) => continue,
                _ => return false,
            }
        }
    }

    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(windows)]
fn components_equivalent(left: std::path::Component<'_>, right: std::path::Component<'_>) -> bool {
    use std::path::Component::*;

    match (left, right) {
        (Prefix(lp), Prefix(rp)) => lp
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&rp.as_os_str().to_string_lossy()),
        (RootDir, RootDir) => true,
        (CurDir, CurDir) => true,
        (ParentDir, ParentDir) => true,
        (Normal(ln), Normal(rn)) => ln
            .to_string_lossy()
            .eq_ignore_ascii_case(&rn.to_string_lossy()),
        _ => false,
    }
}

struct DirectoryListError {
    line_number: usize,
    message: String,
}

#[derive(Default)]
struct DirectoryListStats {
    total_lines: usize,
    useful_lines: usize,
    valid_directories: usize,
    duplicate_directories: usize,
    errors: usize,
}

struct DirectoryListReadResult {
    directories: Vec<PathBuf>,
    errors: Vec<DirectoryListError>,
    stats: DirectoryListStats,
}

fn read_directory_list(
    list_file: &Path,
) -> Result<DirectoryListReadResult, Box<dyn std::error::Error>> {
    let file = File::open(list_file)?;
    let reader = BufReader::new(file);
    let base_dir = list_file.parent().unwrap_or(Path::new("."));
    let mut directories = Vec::new();
    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    let mut stats = DirectoryListStats::default();

    for (line_number, line) in reader.lines().enumerate() {
        let line_number = line_number + 1;
        stats.total_lines += 1;
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                errors.push(DirectoryListError {
                    line_number,
                    message: format!("no se pudo leer la linea: {}", err),
                });
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        stats.useful_lines += 1;

        let raw_path = PathBuf::from(trimmed);
        let resolved = if raw_path.is_absolute() {
            raw_path
        } else {
            base_dir.join(raw_path)
        };
        let canonical = match fs::canonicalize(&resolved) {
            Ok(canonical) => canonical,
            Err(err) => {
                errors.push(DirectoryListError {
                    line_number,
                    message: format!("no se pudo resolver {}: {}", resolved.display(), err),
                });
                stats.errors += 1;
                continue;
            }
        };
        if !canonical.is_dir() {
            errors.push(DirectoryListError {
                line_number,
                message: format!("la ruta no es carpeta: {}", canonical.display()),
            });
            stats.errors += 1;
            continue;
        }
        if seen.insert(canonical.clone()) {
            directories.push(canonical);
            stats.valid_directories += 1;
        } else {
            stats.duplicate_directories += 1;
        }
    }

    Ok(DirectoryListReadResult {
        directories,
        errors,
        stats,
    })
}

fn convert_directory_list_paths(
    input: &Path,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !input.exists() {
        return Err(format!("la ruta no existe: {}", input.display()).into());
    }
    if !input.is_file() {
        return Err(format!("la entrada no es un archivo: {}", input.display()).into());
    }
    if paths_equivalent(input, output) {
        println!(
            "entrada y salida apuntan al mismo archivo: {}",
            output.display()
        );
        return Ok(());
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::copy(input, output)?;
    println!("archivo copiado en: {}", output.display());
    Ok(())
}

struct ChangeLog {
    file: File,
}

impl ChangeLog {
    fn new(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        fs::create_dir_all(root)?;
        let path = root.join("Cambios.txt");
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    fn write_line<S: AsRef<str>>(&mut self, line: S) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "{}", line.as_ref())?;
        self.file.flush()?;
        Ok(())
    }
}

struct FolderAudit {
    file: File,
}

impl FolderAudit {
    fn new(folder: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let audit_dir = folder.to_path_buf();
        fs::create_dir_all(&audit_dir)?;
        let audit_path = audit_dir.join("auditoria.txt");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(audit_path)?;
        Ok(Self { file })
    }

    fn write_line<S: AsRef<str>>(&mut self, line: S) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "{}", line.as_ref())?;
        self.file.flush()?;
        Ok(())
    }
}

struct BackupManifest {
    file: File,
}

impl BackupManifest {
    fn new(run_root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        fs::create_dir_all(run_root)?;
        let manifest_path = run_root.join("manifest.txt");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(manifest_path)?;
        Ok(Self { file })
    }

    fn write_header(
        &mut self,
        source_label: &Path,
        backup_root: &Path,
        work_root: &Path,
        label: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "timestamp={}", timestamp_now())?;
        if let Some(label) = label {
            writeln!(self.file, "label={}", label)?;
        }
        writeln!(self.file, "source_label={}", source_label.display())?;
        writeln!(self.file, "backup_root={}", backup_root.display())?;
        writeln!(self.file, "work_root={}", work_root.display())?;
        self.file.flush()?;
        Ok(())
    }

    fn write_source_stats(
        &mut self,
        stats: &DirectoryListStats,
    ) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "stat\tlineas_totales\t{}", stats.total_lines)?;
        writeln!(self.file, "stat\tregistros_utiles\t{}", stats.useful_lines)?;
        writeln!(
            self.file,
            "stat\tcarpetas_validas\t{}",
            stats.valid_directories
        )?;
        writeln!(
            self.file,
            "stat\tduplicados_omitidos\t{}",
            stats.duplicate_directories
        )?;
        writeln!(self.file, "stat\terrores\t{}", stats.errors)?;
        self.file.flush()?;
        Ok(())
    }

    fn write_summary(&mut self, summary: &RunSummary) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(
            self.file,
            "stat\tcarpetas_intentadas\t{}",
            summary.attempted_folders
        )?;
        writeln!(
            self.file,
            "stat\tcarpetas_procesadas\t{}",
            summary.processed_folders
        )?;
        writeln!(
            self.file,
            "stat\tgrupos_consolidados\t{}",
            summary.merged_groups
        )?;
        writeln!(self.file, "stat\tpdfs_candidatos\t{}", summary.merged_files)?;
        self.file.flush()?;
        Ok(())
    }

    fn write_footer(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "completed_at={}", timestamp_now())?;
        self.file.flush()?;
        Ok(())
    }

    fn write_folder(&mut self, folder: &Path) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "folder\t{}", folder.display())?;
        self.file.flush()?;
        Ok(())
    }

    fn write_output(&mut self, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "output\t{}", output.display())?;
        self.file.flush()?;
        Ok(())
    }

    fn write_backup(
        &mut self,
        original: &Path,
        backup: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(
            self.file,
            "backup\t{}\t{}",
            original.display(),
            backup.display()
        )?;
        self.file.flush()?;
        Ok(())
    }
}

struct RunBackupSession {
    root: PathBuf,
    work_root: PathBuf,
    manifest: BackupManifest,
    copied_paths: HashSet<PathBuf>,
}

impl RunBackupSession {
    fn new(
        source_label: &Path,
        storage_root: &Path,
        label: Option<&str>,
        directory_stats: Option<&DirectoryListStats>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let run_id = if let Some(label) = label {
            validate_backup_label(label)?;
            label.to_string()
        } else {
            format!("run_{}_{}", timestamp_now(), std::process::id())
        };
        let run_root = if let Some(label) = label {
            storage_root.join(BACKUP_DIR_NAME).join(label)
        } else {
            storage_root.join(BACKUP_DIR_NAME).join(&run_id)
        };
        let work_root = storage_root
            .join(BACKUP_DIR_NAME)
            .join("area_trabajo")
            .join(&run_id);

        if let Some(label) = label {
            if label_index_path(label)?.exists() {
                return Err(format!("ya existe un respaldo para la etiqueta: {}", label).into());
            }
        }

        if run_root.exists() {
            if let Some(label) = label {
                return Err(format!("ya existe un respaldo para la etiqueta: {}", label).into());
            }
        }

        let mut manifest = BackupManifest::new(&run_root)?;
        manifest.write_header(source_label, &run_root, &work_root, label)?;
        if let Some(stats) = directory_stats {
            manifest.write_source_stats(stats)?;
        }

        if let Some(label) = label {
            let manifest_path = fs::canonicalize(run_root.join("manifest.txt"))
                .unwrap_or_else(|_| run_root.join("manifest.txt"));
            write_label_index(label, &manifest_path)?;
        }

        Ok(Self {
            root: run_root,
            work_root,
            manifest,
            copied_paths: HashSet::new(),
        })
    }

    fn backup_path_for(&self, original: &Path) -> PathBuf {
        self.root.join("items").join(backup_relative_path(original))
    }

    fn work_dir_for(&self, folder: &Path) -> PathBuf {
        self.work_root.join(backup_relative_path(folder))
    }

    fn record_folder(&mut self, folder: &Path) -> Result<(), Box<dyn std::error::Error>> {
        self.manifest.write_folder(folder)
    }

    fn record_output(&mut self, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
        self.manifest.write_output(output)
    }

    fn backup_file(&mut self, original: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
        if self.copied_paths.insert(original.to_path_buf()) {
            let backup = self.backup_path_for(original);
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(original, &backup)?;
            self.manifest.write_backup(original, &backup)?;
            Ok(backup)
        } else {
            Ok(self.backup_path_for(original))
        }
    }

    fn finish_with_summary(
        &mut self,
        summary: &RunSummary,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.manifest.write_summary(summary)?;
        self.manifest.write_footer()
    }
}

fn backup_relative_path(original: &Path) -> PathBuf {
    let mut relative = PathBuf::new();

    for component in original.components() {
        match component {
            Component::Prefix(prefix) => {
                let text = prefix.as_os_str().to_string_lossy();
                relative.push(format!("drive_{}", sanitize_backup_component(&text)));
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => relative.push("parent"),
            Component::Normal(part) => relative.push(part),
        }
    }

    if relative.as_os_str().is_empty() {
        relative.push("root");
    }

    relative
}

fn sanitize_backup_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            ':' | '\\' | '/' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

fn validate_backup_label(label: &str) -> Result<(), Box<dyn std::error::Error>> {
    if label.is_empty() {
        return Err("la etiqueta no puede estar vacia".into());
    }
    if !label
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(format!("la etiqueta contiene caracteres no permitidos: {}", label).into());
    }
    Ok(())
}

fn label_index_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(env::current_dir()?.join(LABEL_INDEX_DIR))
}

fn label_index_path(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    validate_backup_label(label)?;
    Ok(label_index_dir()?.join(format!("{}.txt", label)))
}

fn backup_storage_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(env::current_dir()?.join(BACKUP_DIR_NAME))
}

fn write_label_index(label: &str, manifest_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let index_path = label_index_path(label)?;
    if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(index_path)?;
    writeln!(file, "label={}", label)?;
    writeln!(file, "manifest={}", manifest_path.display())?;
    writeln!(file, "timestamp={}", timestamp_now())?;
    file.flush()?;
    Ok(())
}

fn resolve_manifest_from_label(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let index_path = label_index_path(label)?;
    if !index_path.exists() {
        return Err(format!("no existe un respaldo indexado para la etiqueta: {}", label).into());
    }

    let file = File::open(&index_path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if let Some(rest) = line.strip_prefix("manifest=") {
            let manifest_path = PathBuf::from(rest);
            if manifest_path.exists() {
                return Ok(manifest_path);
            }
            return Err(format!(
                "el manifest indexado no existe: {}",
                manifest_path.display()
            )
            .into());
        }
    }

    Err(format!("indice de etiqueta invalido: {}", index_path.display()).into())
}

pub(crate) fn cleanup_expired_backups() -> Result<(), Box<dyn std::error::Error>> {
    let backup_root = backup_storage_root()?;
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(BACKUP_RETENTION_SECONDS))
        .unwrap_or(UNIX_EPOCH);
    let mut removed_backups = 0usize;

    if backup_root.exists() {
        for entry in fs::read_dir(&backup_root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            if is_backup_expired(&path, cutoff) {
                if is_safe_backup_path(&backup_root, &path) {
                    fs::remove_dir_all(&path)?;
                    removed_backups += 1;
                    eprintln!("retencion: eliminado respaldo antiguo {}", path.display());
                } else {
                    eprintln!(
                        "retencion: se omite una ruta sospechosa fuera del respaldo base: {}",
                        path.display()
                    );
                }
            }
        }
    }

    let removed_indexes = cleanup_stale_label_indexes()?;
    if removed_backups > 0 || removed_indexes > 0 {
        eprintln!(
            "retencion: limpieza completada (respaldos eliminados={}, indices eliminados={})",
            removed_backups, removed_indexes
        );
    }

    Ok(())
}

fn is_backup_expired(path: &Path, cutoff: SystemTime) -> bool {
    backup_entry_modified_time(path)
        .map(|modified| modified <= cutoff)
        .unwrap_or(false)
}

fn backup_entry_modified_time(path: &Path) -> Option<SystemTime> {
    let manifest_path = path.join("manifest.txt");
    let metadata = if manifest_path.exists() {
        fs::metadata(manifest_path).ok()
    } else {
        fs::metadata(path).ok()
    }?;

    metadata.modified().ok()
}

fn is_safe_backup_path(base: &Path, candidate: &Path) -> bool {
    match (fs::canonicalize(base), fs::canonicalize(candidate)) {
        (Ok(base), Ok(candidate)) => candidate.starts_with(base),
        _ => false,
    }
}

fn cleanup_stale_label_indexes() -> Result<usize, Box<dyn std::error::Error>> {
    let index_dir = label_index_dir()?;
    if !index_dir.exists() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in fs::read_dir(&index_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let manifest_exists = match read_label_index_manifest_path(&path) {
            Ok(Some(manifest_path)) => manifest_path.exists(),
            _ => false,
        };

        if !manifest_exists {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
                eprintln!("retencion: eliminado indice obsoleto {}", path.display());
            }
        }
    }

    Ok(removed)
}

fn read_label_index_manifest_path(
    index_path: &Path,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let file = File::open(index_path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if let Some(rest) = line.strip_prefix("manifest=") {
            return Ok(Some(PathBuf::from(rest)));
        }
    }

    Ok(None)
}

fn detect_group_base(stem: &str) -> Option<&'static str> {
    let normalized = normalize_stem(stem).to_ascii_uppercase();

    if normalized == "RDH" {
        return Some("RHD");
    }

    for base in VALID_NAMES {
        let base_upper = base.to_ascii_uppercase();

        if normalized == base_upper {
            return Some(base);
        }

        if let Some(suffix) = normalized.strip_prefix(&base_upper) {
            if is_allowed_variant_suffix(suffix) {
                return Some(base);
            }
        }
    }

    None
}

fn collect_base_files(files: &[PathBuf], base: &str) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| detect_group_base(stem) == Some(base))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn split_case_variant_duplicates(
    files: Vec<PathBuf>,
    output: &Path,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Some(output_name) = output.file_name().and_then(|s| s.to_str()) else {
        return (files, Vec::new());
    };

    if !files.iter().any(|path| paths_equivalent(path, output)) {
        return (files, Vec::new());
    }

    let mut kept = Vec::with_capacity(files.len());
    let mut duplicates = Vec::new();

    for path in files {
        if paths_equivalent(&path, output) {
            kept.push(path);
            continue;
        }

        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            kept.push(path);
            continue;
        };

        if name.eq_ignore_ascii_case(output_name) {
            duplicates.push(path);
        } else {
            kept.push(path);
        }
    }

    (kept, duplicates)
}

fn describe_files(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn normalize_stem(stem: &str) -> &str {
    let mut end = stem.len();
    for (idx, ch) in stem.char_indices() {
        if matches!(ch, ' ' | '(' | ')' | '[' | ']' | '{' | '}' | '.') {
            end = idx;
            break;
        }
    }
    stem[..end]
        .trim_end_matches(|c: char| c == '_' || c == '-')
        .trim()
}

fn is_allowed_variant_suffix(suffix: &str) -> bool {
    if suffix.is_empty() {
        return false;
    }

    let suffix = suffix.trim();
    let rest = if let Some(rest) = suffix.strip_prefix('_') {
        rest
    } else if let Some(rest) = suffix.strip_prefix('-') {
        rest
    } else {
        return false;
    };

    !rest.trim().is_empty()
}

fn build_aux_paths(
    files: &[PathBuf],
    aux_dir: &Path,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut result = Vec::with_capacity(files.len());
    for (index, _) in files.iter().enumerate() {
        let name = format!("aux_{:03}.pdf", index + 1);
        result.push(aux_dir.join(name));
    }
    Ok(result)
}

fn copy_to_aux(files: &[PathBuf], aux_paths: &[PathBuf]) -> Result<(), Box<dyn std::error::Error>> {
    for (src, dst) in files.iter().zip(aux_paths.iter()) {
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn cleanup_aux(aux_paths: &[PathBuf]) -> Result<(), Box<dyn std::error::Error>> {
    for path in aux_paths {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn delete_sources(files: &[PathBuf], output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for path in files {
        if paths_equivalent(path, output) {
            continue;
        }
        if path.exists() {
            fs::remove_file(path)?;
            println!("  eliminado original: {}", path.display());
        }
    }
    Ok(())
}

fn page_count(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    let doc = Document::load(path)?;
    Ok(doc.get_pages().len() as u32)
}

fn merge_pdfs(inputs: &[PathBuf], output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if inputs.is_empty() {
        return Err("no hay archivos para unir".into());
    }

    let mut merged = Document::with_version("1.5");
    let mut max_id = 1u32;
    let mut collected_pages: Vec<(lopdf::ObjectId, lopdf::Object)> = Vec::new();
    let mut collected_objects: BTreeMap<lopdf::ObjectId, lopdf::Object> = BTreeMap::new();

    for input in inputs {
        let mut doc = Document::load(input)?;
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;

        for page_id in doc.get_pages().into_values() {
            collected_pages.push((page_id, doc.get_object(page_id)?.to_owned()));
        }

        collected_objects.extend(doc.objects);
    }

    let mut catalog_object: Option<(lopdf::ObjectId, lopdf::Object)> = None;
    let mut pages_object: Option<(lopdf::ObjectId, lopdf::Object)> = None;

    for (object_id, object) in collected_objects.into_iter() {
        match object.type_name().unwrap_or(b"") {
            b"Catalog" => {
                catalog_object = Some((
                    if let Some((id, _)) = catalog_object {
                        id
                    } else {
                        object_id
                    },
                    object,
                ));
            }
            b"Pages" => {
                if let Ok(dictionary) = object.as_dict() {
                    let mut dictionary = dictionary.clone();
                    if let Some((_, ref existing_object)) = pages_object {
                        if let Ok(old_dictionary) = existing_object.as_dict() {
                            dictionary.extend(old_dictionary);
                        }
                    }

                    pages_object = Some((
                        if let Some((id, _)) = pages_object {
                            id
                        } else {
                            object_id
                        },
                        lopdf::Object::Dictionary(dictionary),
                    ));
                }
            }
            b"Page" => {}
            b"Outlines" => {}
            b"Outline" => {}
            _ => {
                merged.objects.insert(object_id, object);
            }
        }
    }

    let (catalog_id, catalog_object) = catalog_object.ok_or("Catalog root not found.")?;
    let (pages_id, pages_object) = pages_object.ok_or("Pages root not found.")?;

    if let Ok(dictionary) = pages_object.as_dict() {
        let mut dictionary = dictionary.clone();
        dictionary.set("Count", collected_pages.len() as u32);
        dictionary.set(
            "Kids",
            collected_pages
                .iter()
                .map(|(object_id, _)| lopdf::Object::Reference(*object_id))
                .collect::<Vec<_>>(),
        );
        merged
            .objects
            .insert(pages_id, lopdf::Object::Dictionary(dictionary));
    }

    for (object_id, object) in &collected_pages {
        if let Ok(dictionary) = object.as_dict() {
            let mut dictionary = dictionary.clone();
            dictionary.set("Parent", pages_id);
            merged
                .objects
                .insert(*object_id, lopdf::Object::Dictionary(dictionary));
        }
    }

    if let Ok(dictionary) = catalog_object.as_dict() {
        let mut dictionary = dictionary.clone();
        dictionary.set("Pages", pages_id);
        dictionary.remove(b"Outlines");
        merged
            .objects
            .insert(catalog_id, lopdf::Object::Dictionary(dictionary));
    }

    merged.trailer.set("Root", catalog_id);
    merged.max_id = merged.objects.len() as u32;
    merged.renumber_objects();
    merged.adjust_zero_pages();

    if let Some(outline_id) = merged.build_outline() {
        if let Ok(lopdf::Object::Dictionary(dict)) = merged.get_object_mut(catalog_id) {
            dict.set("Outlines", lopdf::Object::Reference(outline_id));
        }
    }

    merged.save(output)?;
    Ok(())
}

fn read_manifest(manifest_path: &Path) -> Result<ManifestData, Box<dyn std::error::Error>> {
    let file = File::open(manifest_path)?;
    let reader = BufReader::new(file);
    let mut data = ManifestData::default();

    for line in reader.lines() {
        let line = line?;
        if let Some(rest) = line.strip_prefix("timestamp=") {
            data.header.timestamp = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("completed_at=") {
            data.header.completed_at = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("label=") {
            data.header.label = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("source_label=") {
            data.header.source_label = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("backup_root=") {
            data.header.backup_root = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("work_root=") {
            data.header.work_root = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("stat\t") {
            let mut parts = rest.splitn(2, '\t');
            let key = parts.next().unwrap_or_default();
            let value = parts.next().unwrap_or_default();
            if let Ok(parsed) = value.parse::<usize>() {
                match key {
                    "lineas_totales" => data.stats.lineas_totales = Some(parsed),
                    "registros_utiles" => data.stats.registros_utiles = Some(parsed),
                    "carpetas_validas" => data.stats.carpetas_validas = Some(parsed),
                    "duplicados_omitidos" => data.stats.duplicados_omitidos = Some(parsed),
                    "errores" => data.stats.errores = Some(parsed),
                    "carpetas_intentadas" => data.stats.carpetas_intentadas = Some(parsed),
                    "carpetas_procesadas" => data.stats.carpetas_procesadas = Some(parsed),
                    "grupos_consolidados" => data.stats.grupos_consolidados = Some(parsed),
                    "pdfs_candidatos" => data.stats.pdfs_candidatos = Some(parsed),
                    _ => {}
                }
            }
        } else if let Some(rest) = line.strip_prefix("output\t") {
            data.outputs.push(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("folder\t") {
            data.folders.insert(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("backup\t") {
            let mut parts = rest.splitn(2, '\t');
            let original = parts
                .next()
                .ok_or("manifest invalido: falta ruta original")?;
            let backup = parts
                .next()
                .ok_or("manifest invalido: falta ruta respaldo")?;
            data.backups
                .push((PathBuf::from(original), PathBuf::from(backup)));
        }
    }

    Ok(data)
}

fn resolved_manifest_completion_timestamp(
    manifest_path: &Path,
    manifest: &ManifestData,
) -> (Option<String>, bool) {
    if let Some(timestamp) = manifest.header.completed_at.clone() {
        return (Some(timestamp), false);
    }

    if let Some(timestamp) = file_modified_timestamp(manifest_path) {
        return (Some(timestamp), true);
    }

    (None, false)
}

#[derive(Default)]
struct ManifestHeader {
    timestamp: Option<String>,
    completed_at: Option<String>,
    label: Option<String>,
    source_label: Option<PathBuf>,
    backup_root: Option<PathBuf>,
    work_root: Option<PathBuf>,
}

#[derive(Default)]
struct ManifestStats {
    lineas_totales: Option<usize>,
    registros_utiles: Option<usize>,
    carpetas_validas: Option<usize>,
    duplicados_omitidos: Option<usize>,
    errores: Option<usize>,
    carpetas_intentadas: Option<usize>,
    carpetas_procesadas: Option<usize>,
    grupos_consolidados: Option<usize>,
    pdfs_candidatos: Option<usize>,
}

#[derive(Default)]
struct ManifestData {
    header: ManifestHeader,
    stats: ManifestStats,
    outputs: Vec<PathBuf>,
    backups: Vec<(PathBuf, PathBuf)>,
    folders: HashSet<PathBuf>,
}

fn manifest_path_from_target(target: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let target_path = Path::new(target);
    if target_path.exists() {
        if target_path.is_file() {
            Ok(target_path.to_path_buf())
        } else if target_path.is_dir() {
            Ok(target_path.join("manifest.txt"))
        } else {
            Err(format!("ruta de respaldo no valida: {}", target_path.display()).into())
        }
    } else if looks_like_path_target(target) {
        Err(format!("ruta de respaldo no valida: {}", target_path.display()).into())
    } else {
        resolve_manifest_from_label(target)
    }
}

fn report_path_from_target(target: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let manifest_path = manifest_path_from_target(target)?;
    if !manifest_path.exists() {
        return Err(format!("no se encontro manifest: {}", manifest_path.display()).into());
    }

    let report_root = manifest_path.parent().ok_or_else(|| {
        format!(
            "no se puede determinar la carpeta del manifest: {}",
            manifest_path.display()
        )
    })?;
    Ok(report_root.join("reporte_ecuador.html"))
}

fn restore_from_backup(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = manifest_path_from_target(target)?;

    if !manifest_path.exists() {
        return Err(format!("no se encontro manifest: {}", manifest_path.display()).into());
    }

    let manifest = read_manifest(&manifest_path)?;
    if manifest.backups.is_empty() && manifest.outputs.is_empty() {
        return Err(format!("manifest vacio: {}", manifest_path.display()).into());
    }

    let original_paths: HashSet<PathBuf> = manifest
        .backups
        .iter()
        .map(|(original, _)| original.clone())
        .collect();

    for output in manifest.outputs {
        if !original_paths
            .iter()
            .any(|path| paths_equivalent(path, &output))
            && output.exists()
        {
            fs::remove_file(&output)?;
            println!("restauracion: eliminado generado {}", output.display());
        }
    }

    for (original, backup) in manifest.backups {
        if !backup.exists() {
            return Err(format!("falta respaldo: {}", backup.display()).into());
        }
        if let Some(parent) = original.parent() {
            fs::create_dir_all(parent)?;
        }
        if original.exists() {
            fs::remove_file(&original)?;
        }
        fs::copy(&backup, &original)?;
        println!("restauracion: recuperado {}", original.display());
    }

    for folder in manifest.folders {
        let work_dir = manifest
            .header
            .work_root
            .as_ref()
            .map(|root| root.join(backup_relative_path(&folder)))
            .unwrap_or_else(|| folder.join(".he1_aux_temporal"));
        cleanup_generated_artifacts(&folder, &work_dir)?;
    }

    println!("restauracion completada desde {}", manifest_path.display());
    Ok(())
}

fn verify_folder_processed(target: &str, folder: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let report = build_folder_verification_report(target, folder)?;
    println!("{}", report);
    Ok(())
}

pub(crate) fn build_folder_verification_report(
    target: &str,
    folder: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let manifest_path = manifest_path_from_target(target)?;
    if !manifest_path.exists() {
        return Err(format!("no se encontro manifest: {}", manifest_path.display()).into());
    }

    if folder.exists() && !folder.is_dir() {
        return Err(format!("la ruta no es una carpeta: {}", folder.display()).into());
    }

    let manifest = read_manifest(&manifest_path)?;
    let folder_marker = folder.join(".he1_procesado");
    let marker_exists = folder_marker.exists();
    let folder_in_manifest = manifest
        .folders
        .iter()
        .any(|entry| paths_equivalent(entry, folder));
    let outputs_for_folder = manifest
        .outputs
        .iter()
        .filter(|output| {
            output
                .parent()
                .map(|parent| paths_equivalent(parent, folder))
                .unwrap_or(false)
        })
        .count();
    let backups_for_folder = manifest
        .backups
        .iter()
        .filter(|(original, _)| {
            original
                .parent()
                .map(|parent| paths_equivalent(parent, folder))
                .unwrap_or(false)
        })
        .count();

    let label_display = manifest.header.label.as_deref().unwrap_or(target);
    let source_label_display = manifest
        .header
        .source_label
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let backup_root_display = manifest
        .header
        .backup_root
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let started_at = manifest
        .header
        .timestamp
        .as_deref()
        .and_then(timestamp_to_ecuador)
        .unwrap_or_else(|| "no registrado".to_string());
    let (finished_at_timestamp, finished_at_inferred) =
        resolved_manifest_completion_timestamp(&manifest_path, &manifest);
    let finished_at = finished_at_timestamp
        .as_deref()
        .and_then(timestamp_to_ecuador)
        .unwrap_or_else(|| "no registrado".to_string());
    let execution_time = match (
        manifest
            .header
            .timestamp
            .as_deref()
            .and_then(parse_unix_timestamp),
        finished_at_timestamp
            .as_deref()
            .and_then(parse_unix_timestamp),
    ) {
        (Some(start), Some(end)) if end >= start => format_duration_seconds(end - start),
        _ => "no registrado".to_string(),
    };
    let status = match (folder_in_manifest, marker_exists) {
        (true, true) => "procesada y marcada",
        (true, false) => "aparece en el manifest, pero hoy no conserva el marcador",
        (false, true) => "tiene marcador, pero no aparece en el manifest de esta corrida",
        (false, false) => "sin evidencia suficiente de procesamiento para esta etiqueta",
    };

    Ok([
        "verificacion de proceso".to_string(),
        format!("Etiqueta: {}", label_display),
        format!("Manifest: {}", manifest_path.display()),
        format!("Carpeta consultada: {}", folder.display()),
        format!("Estado: {}", status),
        format!(
            "Marcador .he1_procesado: {}",
            if marker_exists {
                folder_marker.display().to_string()
            } else {
                "no encontrado".to_string()
            }
        ),
        format!(
            "Carpeta en manifest: {}",
            if folder_in_manifest { "si" } else { "no" }
        ),
        format!("Salidas asociadas a la carpeta: {}", outputs_for_folder),
        format!("Respaldos asociados a la carpeta: {}", backups_for_folder),
        format!("Inicio Ecuador: {}", started_at),
        format!("Fin Ecuador: {}", finished_at),
        format!("Duracion total: {}", execution_time),
        format!("Fuente original: {}", source_label_display),
        format!("Respaldo raiz: {}", backup_root_display),
        format!(
            "Cierre de corrida: {}",
            if finished_at_inferred {
                "inferido desde la modificacion del manifest"
            } else {
                "registrado en completed_at"
            }
        ),
        "Nota: si la carpeta fue restaurada, el marcador puede desaparecer aunque el manifest siga mostrando la corrida."
            .to_string(),
    ]
    .join("\n"))
}

fn generate_html_report(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = manifest_path_from_target(target)?;
    let report_path = report_path_from_target(target)?;

    let manifest = read_manifest(&manifest_path)?;
    let label_display = manifest.header.label.as_deref().unwrap_or(target);
    let source_label_display = manifest
        .header
        .source_label
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let backup_root_display = manifest
        .header
        .backup_root
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "no registrado".to_string());
    let started_at = manifest
        .header
        .timestamp
        .as_deref()
        .and_then(timestamp_to_ecuador)
        .unwrap_or_else(|| "no registrado".to_string());
    let (finished_at_timestamp, finished_at_inferred) =
        resolved_manifest_completion_timestamp(&manifest_path, &manifest);
    let finished_at = finished_at_timestamp
        .as_deref()
        .and_then(timestamp_to_ecuador)
        .unwrap_or_else(|| "no registrado".to_string());
    let execution_time = match (
        manifest
            .header
            .timestamp
            .as_deref()
            .and_then(parse_unix_timestamp),
        finished_at_timestamp
            .as_deref()
            .and_then(parse_unix_timestamp),
    ) {
        (Some(start), Some(end)) if end >= start => format_duration_seconds(end - start),
        _ => "no registrado".to_string(),
    };
    let generated_at =
        timestamp_to_ecuador(&timestamp_now()).unwrap_or_else(|| "no registrado".to_string());
    let label_index = if let Some(label) = manifest.header.label.as_deref() {
        label_index_path(label)
            .ok()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "no registrado".to_string())
    } else {
        "no registrado".to_string()
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
    let closing_note = if finished_at_inferred {
        "El cierre se infirio desde la fecha de modificacion del manifest porque no quedo registrado completed_at."
    } else {
        "El cierre se tomo del footer completed_at del manifest."
    };

    let html = format!(
        r#"<!doctype html>
<html lang="es">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Reporte de proceso {label}</title>
  <style>
    :root {{
      color-scheme: light;
      --bg: #f6f1e8;
      --panel: #ffffff;
      --ink: #17212b;
      --muted: #5d6b78;
      --accent: #1f5f5b;
      --line: #d7d0c5;
      --chip: #e8f1ef;
    }}
    body {{
      margin: 0;
      font-family: Arial, Helvetica, sans-serif;
      background: linear-gradient(180deg, #f2ede4 0%, #faf8f3 100%);
      color: var(--ink);
    }}
    .wrap {{
      max-width: 980px;
      margin: 0 auto;
      padding: 32px 20px 56px;
    }}
    .hero {{
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 18px;
      padding: 24px;
      box-shadow: 0 12px 34px rgba(23, 33, 43, 0.08);
    }}
    h1 {{
      margin: 0 0 10px;
      font-size: 30px;
    }}
    .subtitle {{
      margin: 0;
      color: var(--muted);
      line-height: 1.45;
    }}
    .grid {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
      gap: 16px;
      margin-top: 18px;
    }}
    .card {{
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 16px;
      padding: 18px;
    }}
    .label {{
      font-size: 12px;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      color: var(--muted);
      margin-bottom: 8px;
    }}
    .value {{
      font-size: 15px;
      line-height: 1.5;
      word-break: break-word;
    }}
    table {{
      width: 100%;
      border-collapse: collapse;
      margin-top: 18px;
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 16px;
      overflow: hidden;
    }}
    th, td {{
      padding: 14px 16px;
      border-bottom: 1px solid var(--line);
      vertical-align: top;
      text-align: left;
    }}
    th {{
      width: 260px;
      background: #f8f4ec;
      color: var(--accent);
    }}
    tr:last-child th, tr:last-child td {{
      border-bottom: 0;
    }}
    .chip {{
      display: inline-block;
      padding: 6px 10px;
      border-radius: 999px;
      background: var(--chip);
      color: var(--accent);
      font-size: 13px;
      font-weight: 700;
    }}
    .foot {{
      margin-top: 18px;
      color: var(--muted);
      font-size: 13px;
    }}
    code {{
      font-family: Consolas, "Courier New", monospace;
      font-size: 0.95em;
    }}
  </style>
</head>
<body>
  <main class="wrap">
    <section class="hero">
      <span class="chip">Hora de Ecuador continental (UTC-5)</span>
      <h1>Reporte de proceso</h1>
      <p class="subtitle">
        Etiqueta <strong>{label}</strong>. Este reporte resume cuándo se procesó la corrida y dónde quedó registrado.
      </p>
      <div class="grid">
        <div class="card">
          <div class="label">Inicio en Ecuador</div>
          <div class="value">{started_at}</div>
        </div>
        <div class="card">
          <div class="label">Fin en Ecuador</div>
          <div class="value">{finished_at}</div>
        </div>
        <div class="card">
          <div class="label">Reporte generado</div>
          <div class="value">{generated_at}</div>
        </div>
        <div class="card">
          <div class="label">Duraci&oacute;n total</div>
          <div class="value">{execution_time}</div>
        </div>
      </div>
    </section>

    <table>
      <tr>
        <th>Etiqueta</th>
        <td>{label}</td>
      </tr>
      <tr>
        <th>Fuente original</th>
        <td>{source_label}</td>
      </tr>
      <tr>
        <th>Respaldo</th>
        <td>{backup_root}</td>
      </tr>
      <tr>
        <th>Indice de etiqueta</th>
        <td><code>{label_index}</code></td>
      </tr>
      <tr>
        <th>Manifest</th>
        <td><code>{manifest_path}</code></td>
      </tr>
      <tr>
        <th>Carpetas procesadas</th>
        <td>{folder_count}</td>
      </tr>
      <tr>
        <th>Archivos respaldados</th>
        <td>{backup_count}</td>
      </tr>
      <tr>
        <th>Salidas registradas</th>
        <td>{output_count}</td>
      </tr>
      <tr>
        <th>TXT lineas totales</th>
        <td>{source_total_lines}</td>
      </tr>
      <tr>
        <th>TXT registros utiles</th>
        <td>{source_useful_lines}</td>
      </tr>
      <tr>
        <th>TXT carpetas validas</th>
        <td>{source_valid_directories}</td>
      </tr>
      <tr>
        <th>TXT duplicados omitidos</th>
        <td>{source_duplicate_directories}</td>
      </tr>
      <tr>
        <th>TXT errores</th>
        <td>{source_errors}</td>
      </tr>
      <tr>
        <th>Carpetas intentadas</th>
        <td>{attempted_folders}</td>
      </tr>
      <tr>
        <th>Carpetas procesadas</th>
        <td>{processed_folders}</td>
      </tr>
      <tr>
        <th>Grupos consolidados</th>
        <td>{merged_groups}</td>
      </tr>
      <tr>
        <th>PDFs candidatos</th>
        <td>{pdf_candidates}</td>
      </tr>
    </table>

    <p class="foot">{closing_note}</p>
    <p class="foot">
      El archivo HTML se generó desde la informacion guardada por el programa y puede abrirse sin depender del binario.
    </p>
  </main>
</body>
</html>
"#,
        label = escape_html(label_display),
        started_at = escape_html(&started_at),
        finished_at = escape_html(&finished_at),
        execution_time = escape_html(&execution_time),
        generated_at = escape_html(&generated_at),
        source_label = escape_html(&source_label_display),
        backup_root = escape_html(&backup_root_display),
        label_index = escape_html(&label_index),
        manifest_path = escape_html(&manifest_path.display().to_string()),
        folder_count = manifest.folders.len(),
        backup_count = manifest.backups.len(),
        output_count = manifest.outputs.len(),
        source_total_lines = escape_html(&source_total_lines),
        source_useful_lines = escape_html(&source_useful_lines),
        source_valid_directories = escape_html(&source_valid_directories),
        source_duplicate_directories = escape_html(&source_duplicate_directories),
        source_errors = escape_html(&source_errors),
        attempted_folders = escape_html(&attempted_folders),
        processed_folders = escape_html(&processed_folders),
        merged_groups = escape_html(&merged_groups),
        pdf_candidates = escape_html(&pdf_candidates),
        closing_note = escape_html(closing_note),
    );

    fs::write(&report_path, html)?;
    println!("reporte HTML generado en: {}", report_path.display());
    Ok(())
}

fn looks_like_path_target(value: &str) -> bool {
    value.contains('\\') || value.contains('/') || value.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_single_component_uses_current_dir_as_root() {
        let source_label = Path::new("folder_0001");
        let log_root = log_root_for(source_label);

        assert_eq!(log_root, Path::new("."));
    }

    #[test]
    fn fuentes_txt_and_pdfs_use_project_root_as_storage_root() {
        #[cfg(windows)]
        {
            let txt_source = Path::new(r"G:\repo\fuentes_txt\PATH_DIRECTORIOS_1.txt");
            let pdf_source = Path::new(r"G:\repo\pdfs\folder_0101");

            assert_eq!(log_root_for(txt_source), PathBuf::from(r"G:\repo"));
            assert_eq!(log_root_for(pdf_source), PathBuf::from(r"G:\repo"));
        }

        #[cfg(not(windows))]
        {
            let txt_source = Path::new("/repo/fuentes_txt/PATH_DIRECTORIOS_1.txt");
            let pdf_source = Path::new("/repo/pdfs/folder_0101");

            assert_eq!(log_root_for(txt_source), PathBuf::from("/repo"));
            assert_eq!(log_root_for(pdf_source), PathBuf::from("/repo"));
        }
    }

    #[test]
    fn path_equivalence_matches_windows_case_insensitive_paths() {
        #[cfg(windows)]
        {
            assert!(paths_equivalent(
                Path::new(r"G:\repo\PI.pdf"),
                Path::new(r"g:\REPO\pi.pdf")
            ));
        }
    }

    #[test]
    fn backup_label_validation_accepts_safe_label() {
        assert!(validate_backup_label("folder_0101").is_ok());
        assert!(validate_backup_label("folder-0101.v1").is_ok());
    }

    #[test]
    fn backup_label_validation_rejects_unsafe_label() {
        assert!(validate_backup_label("folder 0101").is_err());
        assert!(validate_backup_label("folder/0101").is_err());
    }

    #[test]
    fn restore_target_detection_distinguishes_labels_from_paths() {
        assert!(looks_like_path_target(r"G:\repo\respaldos\manifest.txt"));
        assert!(looks_like_path_target(r"folder\sub"));
        assert!(!looks_like_path_target("folder_0101"));
    }
}
