use lopdf::Document;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const VALID_NAMES: &[&str] = &[
    "PI", "CC", "CV", "AES", "053", "006", "007", "017", "018", "018A", "113", "114", "115",
    "ORS", "002", "010A", "010B", "012A", "012B", "033", "013A", "013B", "PTR", "RTR", "08",
    "008",
    "FSCS", "FSICS", "FRDCS", "ANX2", "HR", "RHD", "IMT", "CEC", "RAD", "ITS", "RVD", "119",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return Ok(());
    }
    if args.first().map(|s| s.as_str()) == Some("--restore") {
        return Err("la opcion --restore fue retirada; ya no se usan auxiliares".into());
    }

    let root_arg = args
        .first()
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    let root = PathBuf::from(root_arg);

    if !root.exists() {
        return Err(format!("ruta no existe: {}", root.display()).into());
    }

    let mut folders = Vec::new();
    collect_folders(&root, &mut folders)?;

    let mut total_groups = 0usize;
    let mut total_files = 0usize;
    let mut change_log = ChangeLog::new(&root)?;
    change_log.write_line(format!("INICIO corrida: {}", timestamp_now()))?;
    change_log.write_line("FASE 1: unificacion por reglas canónicas")?;

    for folder in folders {
        let processed_marker = folder.join(".he1_procesado");
        if processed_marker.exists() {
            println!("saltando carpeta ya procesada: {}", folder.display());
            change_log.write_line(format!("CARPETA OMITIDA (ya procesada): {}", folder.display()))?;
            continue;
        }

        println!("analizando carpeta: {}", folder.display());
        change_log.write_line(format!("CARPETA: {}", folder.display()))?;
        let mut pdf_files = Vec::new();
        let mut pdf_count = 0usize;
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("pdf")) != Some(true) {
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
            }
        }

        println!("  PDFs encontrados: {}", pdf_count);
        change_log.write_line(format!("  PDFs encontrados: {}", pdf_count))?;

        for (file_name, base) in ignored_files {
            change_log.write_line(format!("  OBSERVADO: {} | base {}", file_name, base))?;
        }

        for base in VALID_NAMES {
            let mut files = collect_base_files(&pdf_files, base);
            if files.is_empty() {
                continue;
            }
            files.sort();
            println!("  base {} con {} archivos", base, files.len());
            change_log.write_line(format!("  base {} con {} archivos", base, files.len()))?;
            change_log.write_line(format!("    antes: {}", describe_files(&files)))?;

            if files.len() == 1 {
                let source = &files[0];
                let output = canonical_output_path(&folder, base);
                if source == &output {
                    println!("  base {} ya consolidada en {}", base, output.display());
                    change_log.write_line(format!("    despues: {}", output.display()))?;
                    change_log.write_line("    accion: omitido para no reprocesar el archivo unificado")?;
                    continue;
                }
                if output.exists() {
                    fs::remove_file(&output)?;
                }
                fs::rename(source, &output)?;
                println!("  base {} renombrada directo a {}", base, output.display());
                change_log.write_line(format!("    despues: {}", output.display()))?;
                change_log.write_line("    accion: renombrado directo sin union")?;
                continue;
            }

            total_groups += 1;
            total_files += files.len();
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

            let aux_dir = folder.join(".he1_aux_temporal").join(base);
            fs::create_dir_all(&aux_dir)?;
            let aux_paths = build_aux_paths(&files, &aux_dir)?;
            copy_to_aux(&files, &aux_paths)?;
            let output = canonical_output_path(&folder, base);
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
                return Err(format!(
                    "verificacion fallo para {}: paginas fuente={} paginas salida={}",
                    output.display(),
                    source_pages,
                    merged_pages
                )
                .into());
            }

            delete_sources(&files, &output)?;
            cleanup_aux(&aux_paths)?;
            if aux_dir.read_dir()?.next().is_none() {
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
            for (path, pages, size) in per_file_pages {
                change_log.write_line(format!(
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
            change_log.write_line("    estado: completado con exito")?;
        }

        fs::write(&processed_marker, format!("procesado_en={}\n", timestamp_now()))?;
        change_log.write_line(format!("MARCA CREADA: {}", processed_marker.display()))?;
    }

    println!(
        "resumen: {} grupos procesados, {} PDFs candidatos",
        total_groups, total_files
    );
    change_log.write_line(format!(
        "RESUMEN: {} grupos procesados, {} PDFs candidatos",
        total_groups, total_files
    ))?;
    change_log.write_line(format!("FIN corrida: {}", timestamp_now()))?;

    Ok(())
}

fn print_usage() {
    println!("uso:");
    println!("  he1-unificar-pdfs <ruta_raiz>");
    println!();
    println!("ejemplo:");
    println!("  he1-unificar-pdfs /datos/planillas");
    println!();
    println!("comportamiento:");
    println!("  - recorre carpetas dentro de la ruta indicada");
    println!("  - agrupa PDFs por nombre base valido");
    println!("  - genera <base>.pdf sin modificar los originales");
    println!("  - deja un log Cambios.txt con el detalle de la ejecucion");
    println!("  - si existe base.pdf y variantes como base123.pdf, se unen y base.pdf queda como salida final");
    println!();
    println!("ejemplos extremos:");
    println!("  - PI.pdf + PI13.pdf + PI5554.pdf -> PI.pdf");
    println!("  - CC.pdf + cC_01.pdf + CC04.pdf + CC_2.pdf -> CC.pdf");
    println!("  - 018A.pdf + 018A_99.pdf + 018A7.pdf -> 018A.pdf");
    println!("  - 010B.pdf + 010B_1.pdf + 010B99.pdf -> 010B.pdf");
    println!("  - si solo existe PI12234.pdf -> PI.pdf");
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

struct ChangeLog {
    file: std::fs::File,
}

impl ChangeLog {
    fn new(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let path = root.join("Cambios.txt");
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    fn write_line<S: AsRef<str>>(&mut self, line: S) -> Result<(), Box<dyn std::error::Error>> {
        writeln!(self.file, "{}", line.as_ref())?;
        Ok(())
    }
}

fn collect_folders(root: &Path, folders: &mut Vec<PathBuf>) -> std::io::Result<()> {
    folders.push(root.to_path_buf());
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_folders(&path, folders)?;
        }
    }
    Ok(())
}

fn detect_group_base(stem: &str) -> Option<&'static str> {
    let normalized = normalize_stem(stem);

    if normalized.eq_ignore_ascii_case("RDH") {
        return Some("RHD");
    }

    for base in VALID_NAMES {
        if normalized.eq_ignore_ascii_case(base) {
            return Some(base);
        }

        if let Some(suffix) = normalized.strip_prefix(base) {
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
    stem[..end].trim_end_matches(|c: char| c == '_' || c == '-' ).trim()
}

fn is_allowed_variant_suffix(suffix: &str) -> bool {
    if suffix.is_empty() {
        return false;
    }

    let suffix = suffix.trim();
    if !suffix.starts_with('_') {
        return false;
    }

    let rest = suffix.trim_start_matches('_').trim();
    !rest.is_empty()
}

fn build_aux_paths(files: &[PathBuf], aux_dir: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
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

fn delete_sources(
    files: &[PathBuf],
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    for path in files {
        if path == output {
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

    let mut command = Command::new("pdfunite");
    for input in inputs {
        command.arg(input);
    }
    command.arg(output);

    let status = command.status()?;
    if !status.success() {
        return Err(format!("pdfunite fallo para {}", output.display()).into());
    }
    Ok(())
}
