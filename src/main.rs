use lopdf::Document;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const VALID_NAMES: &[&str] = &[
    "PI", "CC", "CV", "AES", "053", "006", "007", "017", "018", "018A", "113", "114", "115", "ORS",
    "002", "010A", "010B", "012A", "012B", "033", "013A", "013B", "PTR", "RTR", "08", "008",
    "FSCS", "FSICS", "FRDCS", "ANX2", "HR", "RHD", "IMT", "CEC", "RAD", "ITS", "RVD", "119",
];

const LABEL_INDEX_DIR: &str = ".he1_label_index";

#[derive(Debug)]
enum AppMode {
    Process { input: PathBuf, label: String },
    Restore(String),
    Help,
}

#[derive(Default)]
struct FolderSummary {
    merged_groups: usize,
    merged_files: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match parse_args()? {
        AppMode::Help => {
            print_usage();
            Ok(())
        }
        AppMode::Restore(target) => restore_from_backup(&target),
        AppMode::Process { input, label } => run_process_mode(&input, &label),
    }
}

fn parse_args() -> Result<AppMode, Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(AppMode::Help);
    }

    if args.first().map(|s| s.as_str()) == Some("--restore") {
        if args.len() != 2 {
            return Err("uso: he1-unificar-pdfs --restore <etiqueta | ruta_respaldo_o_manifest.txt>".into());
        }
        return Ok(AppMode::Restore(args[1].clone()));
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

fn run_process_mode(input: &Path, label: &str) -> Result<(), Box<dyn std::error::Error>> {
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
        run_directory_list(input, Some(label))
    } else if input.is_dir() {
        run_directories(vec![input.to_path_buf()], input, Some(label), &[])
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
) -> Result<(), Box<dyn std::error::Error>> {
    let DirectoryListReadResult {
        directories,
        errors,
    } = read_directory_list(list_file)?;
    if directories.is_empty() {
        if !errors.is_empty() {
            println!("errores en el archivo de directorios:");
            for error in &errors {
                println!("  linea {}: {}", error.line_number, error.message);
            }
        }
        return Err(format!(
            "el archivo de directorios no contiene rutas validas: {}",
            list_file.display()
        )
        .into());
    }

    if !errors.is_empty() {
        println!(
            "se omitieron {} linea(s) con error; quedan registradas en el log",
            errors.len()
        );
    }

    run_directories(directories, list_file, label, &errors)
}

fn run_directories(
    directories: Vec<PathBuf>,
    source_label: &Path,
    label: Option<&str>,
    directory_errors: &[DirectoryListError],
) -> Result<(), Box<dyn std::error::Error>> {
    let log_root = log_root_for(source_label);
    let mut change_log = ChangeLog::new(log_root)?;
    let mut backup_session = RunBackupSession::new(source_label, log_root, label)?;

    change_log.write_line(format!("INICIO corrida: {}", timestamp_now()))?;
    change_log.write_line(format!("FUENTE: {}", source_label.display()))?;
    if let Some(label) = label {
        change_log.write_line(format!("ETIQUETA: {}", label))?;
    }
    change_log.write_line(format!(
        "RESPALDO: {}",
        backup_session.root.display()
    ))?;
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
    }
    change_log.write_line("FASE 1: unificacion por reglas canonicas")?;

    let mut total_groups = 0usize;
    let mut total_files = 0usize;
    let mut processed_folders = 0usize;

    for folder in directories {
        match process_folder(&folder, &mut change_log, &mut backup_session) {
            Ok(summary) => {
                total_groups += summary.merged_groups;
                total_files += summary.merged_files;
                processed_folders += 1;
            }
            Err(err) => {
                println!("error en carpeta {}: {}", folder.display(), err);
                change_log.write_line(format!(
                    "ERROR carpeta {}: {}",
                    folder.display(),
                    err
                ))?;
            }
        }
    }

    println!(
        "resumen: {} carpetas procesadas, {} grupos consolidados, {} PDFs candidatos",
        processed_folders, total_groups, total_files
    );
    change_log.write_line(format!(
        "RESUMEN: {} carpetas procesadas, {} grupos consolidados, {} PDFs candidatos",
        processed_folders, total_groups, total_files
    ))?;
    change_log.write_line(format!("FIN corrida: {}", timestamp_now()))?;

    Ok(())
}

fn process_folder(
    folder: &Path,
    change_log: &mut ChangeLog,
    backup_session: &mut RunBackupSession,
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
        }

        fs::write(
            &processed_marker,
            format!("procesado_en={}\n", timestamp_now()),
        )?;
        change_log.write_line(format!("MARCA CREADA: {}", processed_marker.display()))?;
        folder_audit.write_line(format!("MARCA CREADA: {}", processed_marker.display()))?;
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
    println!();
    println!("ejemplo:");
    println!("  he1-unificar-pdfs --label folder_0001 G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\pdfs\\folder_0001");
    println!("  he1-unificar-pdfs --restore folder_0001");
    println!("  he1-unificar-pdfs --restore G:\\codex_projects\\rust_cambia_nombre_planillas_he1\\fuentes_txt\\.he1_respaldo\\run_...\\manifest.txt");
    println!();
    println!("comportamiento:");
    println!("  - si la entrada es .txt, cada linea valida se interpreta como carpeta");
    println!("  - si la entrada es carpeta, se procesa esa carpeta como unidad");
    println!("  - solo se procesan archivos PDF que cumplan la regla canonica");
    println!("  - los originales que se tocan se respaldan para poder restaurar");
    println!("  - la salida final queda en la misma carpeta original");
    println!("  - el respaldo se guarda en una carpeta oculta .he1_respaldo");
    println!("  - --restore reconstruye los originales y elimina los PDFs generados");
    println!("  - deja un log Cambios.txt con el detalle de la ejecucion");
    println!("  - documentacion de reglas: ver REGLA_UNIFICACION.md");
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

fn canonical_output_path(folder: &Path, base: &str) -> PathBuf {
    folder.join(format!("{}.pdf", base))
}

fn log_root_for(source_label: &Path) -> &Path {
    match source_label.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
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

struct DirectoryListReadResult {
    directories: Vec<PathBuf>,
    errors: Vec<DirectoryListError>,
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

    for (line_number, line) in reader.lines().enumerate() {
        let line_number = line_number + 1;
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
                continue;
            }
        };
        if !canonical.is_dir() {
            errors.push(DirectoryListError {
                line_number,
                message: format!("la ruta no es carpeta: {}", canonical.display()),
            });
            continue;
        }
        if seen.insert(canonical.clone()) {
            directories.push(canonical);
        }
    }

    Ok(DirectoryListReadResult {
        directories,
        errors,
    })
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
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let run_root = if let Some(label) = label {
            validate_backup_label(label)?;
            storage_root.join(".he1_respaldo").join(label)
        } else {
            storage_root
                .join(".he1_respaldo")
                .join(format!("run_{}_{}", timestamp_now(), std::process::id()))
        };

        if run_root.exists() {
            if let Some(label) = label {
                return Err(format!("ya existe un respaldo para la etiqueta: {}", label).into());
            }
        }

        let mut manifest = BackupManifest::new(&run_root)?;
        manifest.write_header(source_label, &run_root, label)?;

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
        if let Some(rest) = line.strip_prefix("output\t") {
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

#[derive(Default)]
struct ManifestData {
    outputs: Vec<PathBuf>,
    backups: Vec<(PathBuf, PathBuf)>,
    folders: HashSet<PathBuf>,
}

fn restore_from_backup(target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let target_path = Path::new(target);
    let manifest_path = if target_path.exists() {
        if target_path.is_file() {
            target_path.to_path_buf()
        } else if target_path.is_dir() {
            target_path.join("manifest.txt")
        } else {
            return Err(format!("ruta de respaldo no valida: {}", target_path.display()).into());
        }
    } else if looks_like_path_target(target) {
        return Err(format!("ruta de respaldo no valida: {}", target_path.display()).into());
    } else {
        resolve_manifest_from_label(target)?
    };

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
