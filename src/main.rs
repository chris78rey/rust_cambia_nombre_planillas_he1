use lopdf::Document;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
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
    if let Err(err) = cleanup_expired_backups() {
        eprintln!("advertencia: no se pudo aplicar la retencion de respaldos: {}", err);
    }
    match parse_args()? {
        AppMode::Help => {
            print_usage();
            Ok(())
        }
        AppMode::Restore(target) => restore_from_backup(&target),
        AppMode::Report(target) => generate_html_report(&target),
        AppMode::Check { target, folder } => verify_folder_processed(&target, &folder),
        AppMode::ConvertPaths { input, output } => convert_directory_list_paths(&input, &output),
        AppMode::Telegram => telegram::run(),
        AppMode::Process { input, label } => run_process_mode(&input, &label, None),
    }
}

fn parse_args() -> Result<AppMode, Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(AppMode::Help);
    }

    if args.first().map(|s| s.as_str()) == Some("--telegram") {
        if args.len() != 1 {
            return Err("uso: he1-unificar-pdfs --telegram".into());
        }
        return Ok(AppMode::Telegram);
    }

    if args.first().map(|s| s.as_str()) == Some("--restore") {
        if args.len() != 2 {
            return Err("uso: he1-unificar-pdfs --restore <etiqueta | ruta_respaldo_o_manifest.txt>".into());
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
            return Err("uso: he1-unificar-pdfs --report <etiqueta | ruta_manifest_o_respaldo>".into());
        }
        return Ok(AppMode::Report(args[1].clone()));
    }

    if matches!(
        args.first().map(|s| s.as_str()),
        Some("--convert-paths" | "--convertir-rutas" | "--windows-to-linux")
    ) {
        if args.len() != 3 {
            return Err(
                "uso: he1-unificar-pdfs --convert-paths <entrada.txt> <salida.txt>".into(),
            );
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
        run_directories(vec![input.to_path_buf()], input, Some(label), &[], None, progress)
    } else {
        Err(format!(
            "la entrada debe ser una carpeta o un archivo .txt: {}",
            input.display()
        )
        .into())
    }
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

    run_directories(directories, list_file, label, &errors, Some(&stats), progress)
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
    let mut backup_session = RunBackupSession::new(source_label, &log_root, label, directory_stats)?;
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
    change_log.write_line(format!(
        "RESPALDO: {}",
        backup_session.root.display()
    ))?;
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
                error.line_number,
                error.message
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
                        folder_number,
                        total_folders,
                        summary.merged_groups,
                        summary.renamed_files
                    ),
                );
            }
            Err(err) => {
                println!("error en carpeta {}: {}", folder.display(), err);
                change_log.write_line(format!(
                    "ERROR carpeta {}: {}",
                    folder.display(),
                    err
                ))?;
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
    let mut folder_audit = FolderAudit::new(folder)?;
    folder_audit.write_line(format!("AUDITORIA: {}", folder.display()))?;
    folder_audit.write_line(format!("INICIO: {}", timestamp_now()))?;
    folder_audit.write_line(format!(
        "RUTA_AUX: {}",
        folder.join(".he1_aux_temporal").display()
    ))?;

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
        format!("carpeta {}: {} PDF(s) detectados", folder.display(), pdf_count),
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
            if files.len() == 1 {
                let source = &files[0];
                if paths_equivalent(source, &output) {
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
                backup_path_if_needed(backup_session, &mut folder_backups, source)?;
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
            for path in &files {
                backup_path_if_needed(backup_session, &mut folder_backups, path)?;
            }
            if output.exists() && !files.iter().any(|p| paths_equivalent(p, &output)) {
                backup_path_if_needed(backup_session, &mut folder_backups, &output)?;
            }

            let aux_dir = folder.join(".he1_aux_temporal").join(base);
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

            delete_sources(&files, &output)?;
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

    cleanup_generated_artifacts(folder)?;
    Ok(())
}

fn cleanup_generated_artifacts(folder: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let processed_marker = folder.join(".he1_procesado");
    if processed_marker.exists() {
        fs::remove_file(processed_marker)?;
    }

    let aux_dir = folder.join(".he1_aux_temporal");
    if aux_dir.exists() {
        for entry in fs::read_dir(&aux_dir)? {
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
    println!("  he1-unificar-pdfs --telegram");
    println!();
    println!("ejemplo:");
    println!("  he1-unificar-pdfs --label folder_0001 G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\pdfs\\folder_0001");
    println!("  he1-unificar-pdfs --restore folder_0001");
    println!("  he1-unificar-pdfs --report folder_0001");
    println!("  he1-unificar-pdfs --check folder_0001 G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\pdfs\\folder_0001");
    println!("  he1-unificar-pdfs --convert-paths G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\fuentes_txt\\PATH_DIRECTORIOS.txt G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\fuentes_txt\\PATH_DIRECTORIOS.linux.txt");
    println!("  he1-unificar-pdfs --restore G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\he1_respaldo\\run_...\\manifest.txt");
    println!("  he1-unificar-pdfs --report G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\he1_respaldo\\run_...\\manifest.txt");
    println!("  he1-unificar-pdfs --check G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\he1_respaldo\\run_...\\manifest.txt G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\pdfs\\folder_0001");
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
    println!("  - comandos: /process <etiqueta>, /process_report <etiqueta>, /restore <etiqueta>, /report <etiqueta>");
    println!();
    println!("regla de nombres:");
    println!("  - acepta base.pdf");
    println!("  - acepta variantes base_*.pdf o base-*.pdf");
    println!("  - tambien acepta nombres que se reducen al canonico por espacios, parentesis, corchetes, llaves o puntos");
    println!("  - la comparacion no distingue mayusculas/minusculas; RHd-copia_extra.PDF puede entrar como RHD.pdf");
    println!("  - no acepta nombres pegados sin _ o - como PI13.pdf o AES4545.pdf");
}

fn timestamp_now() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => format!("{}", dur.as_secs()),
        Err(_) => "0".to_string(),
    }
}

const ECUADOR_OFFSET_SECONDS: i64 = -5 * 3600;

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
        println!("entrada y salida apuntan al mismo archivo: {}", output.display());
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
        let audit_dir = folder.join(".he1_aux_temporal");
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
        label: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "timestamp={}", timestamp_now())?;
        if let Some(label) = label {
            writeln!(self.file, "label={}", label)?;
        }
        writeln!(self.file, "source_label={}", source_label.display())?;
        writeln!(self.file, "backup_root={}", backup_root.display())?;
        self.file.flush()?;
        Ok(())
    }

    fn write_source_stats(
        &mut self,
        stats: &DirectoryListStats,
    ) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "stat\tlineas_totales\t{}", stats.total_lines)?;
        writeln!(self.file, "stat\tregistros_utiles\t{}", stats.useful_lines)?;
        writeln!(self.file, "stat\tcarpetas_validas\t{}", stats.valid_directories)?;
        writeln!(self.file, "stat\tduplicados_omitidos\t{}", stats.duplicate_directories)?;
        writeln!(self.file, "stat\terrores\t{}", stats.errors)?;
        self.file.flush()?;
        Ok(())
    }

    fn write_summary(
        &mut self,
        summary: &RunSummary,
    ) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "stat\tcarpetas_intentadas\t{}", summary.attempted_folders)?;
        writeln!(self.file, "stat\tcarpetas_procesadas\t{}", summary.processed_folders)?;
        writeln!(self.file, "stat\tgrupos_consolidados\t{}", summary.merged_groups)?;
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
        let run_root = if let Some(label) = label {
            validate_backup_label(label)?;
            storage_root.join(BACKUP_DIR_NAME).join(label)
        } else {
            storage_root
                .join(BACKUP_DIR_NAME)
                .join(format!("run_{}_{}", timestamp_now(), std::process::id()))
        };

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
        manifest.write_header(source_label, &run_root, label)?;
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
            manifest,
            copied_paths: HashSet::new(),
        })
    }

    fn backup_path_for(&self, original: &Path) -> PathBuf {
        self.root.join("items").join(backup_relative_path(original))
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
        return Err(format!(
            "la etiqueta contiene caracteres no permitidos: {}",
            label
        )
        .into());
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
        return Err(format!(
            "no existe un respaldo indexado para la etiqueta: {}",
            label
        )
        .into());
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

fn read_label_index_manifest_path(index_path: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
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

    let report_root = manifest_path
        .parent()
        .ok_or_else(|| format!("no se puede determinar la carpeta del manifest: {}", manifest_path.display()))?;
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

    let original_paths: HashSet<PathBuf> =
        manifest.backups.iter().map(|(original, _)| original.clone()).collect();

    for output in manifest.outputs {
        if !original_paths.iter().any(|path| paths_equivalent(path, &output)) && output.exists() {
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
        cleanup_generated_artifacts(&folder)?;
    }

    println!("restauracion completada desde {}", manifest_path.display());
    Ok(())
}

fn verify_folder_processed(
    target: &str,
    folder: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
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
        manifest.header.timestamp.as_deref().and_then(parse_unix_timestamp),
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
    let label_display = manifest
        .header
        .label
        .as_deref()
        .unwrap_or(target);
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
        manifest.header.timestamp.as_deref().and_then(parse_unix_timestamp),
        finished_at_timestamp
            .as_deref()
            .and_then(parse_unix_timestamp),
    ) {
        (Some(start), Some(end)) if end >= start => format_duration_seconds(end - start),
        _ => "no registrado".to_string(),
    };
    let generated_at = timestamp_to_ecuador(&timestamp_now()).unwrap_or_else(|| "no registrado".to_string());
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
        let txt_source = Path::new(r"G:\repo\fuentes_txt\PATH_DIRECTORIOS_1.txt");
        let pdf_source = Path::new(r"G:\repo\pdfs\folder_0101");

        assert_eq!(log_root_for(txt_source), PathBuf::from(r"G:\repo"));
        assert_eq!(log_root_for(pdf_source), PathBuf::from(r"G:\repo"));
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
