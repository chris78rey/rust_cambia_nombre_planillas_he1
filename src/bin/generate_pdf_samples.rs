use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_OUTPUT_DIR: &str = "pdfs";
const DEFAULT_FOLDER_COUNT: usize = 1000;

const VALID_NAMES: &[&str] = &[
    "PI", "CC", "CV", "AES", "053", "006", "007", "017", "018", "018A", "113", "114", "115", "ORS",
    "002", "010A", "010B", "012A", "012B", "033", "013A", "013B", "PTR", "RTR", "08", "008",
    "FSCS", "FSICS", "FRDCS", "ANX2", "HR", "RHD", "IMT", "CEC", "RAD", "ITS", "RVD", "119",
];

#[derive(Clone, Debug)]
struct SampleFile {
    file_name: String,
    title: String,
    pages: usize,
}

#[derive(Debug)]
struct Config {
    output_dir: PathBuf,
    folder_count: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let config = parse_args()?;
    prepare_output_dir(&config.output_dir)?;

    for folder_index in 1..=config.folder_count {
        let folder_name = format!("folder_{:04}", folder_index);
        let folder_path = config.output_dir.join(folder_name);
        fs::create_dir_all(&folder_path)?;

        for sample in build_folder_plan(folder_index) {
            let path = folder_path.join(&sample.file_name);
            write_pdf(&path, &sample.title, sample.pages)?;
        }
    }

    println!(
        "dataset creado en {} con {} carpetas",
        config.output_dir.display(),
        config.folder_count
    );
    Ok(())
}

fn parse_args() -> Result<Config, Box<dyn Error>> {
    let mut output_dir = PathBuf::from(DEFAULT_OUTPUT_DIR);
    let mut folder_count = DEFAULT_FOLDER_COUNT;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" | "-o" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--output requiere una ruta".to_string())?;
                output_dir = PathBuf::from(value);
            }
            "--folders" | "-n" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--folders requiere un numero".to_string())?;
                folder_count = value
                    .parse::<usize>()
                    .map_err(|_| format!("numero invalido para --folders: {}", value))?;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!("argumento desconocido: {}", other).into());
            }
        }
    }

    if folder_count == 0 {
        return Err("--folders debe ser mayor que cero".into());
    }

    Ok(Config {
        output_dir,
        folder_count,
    })
}

fn print_usage() {
    println!("uso:");
    println!("  generate_pdf_samples [--output pdfs] [--folders 1000]");
    println!();
    println!("objetivo:");
    println!(
        "  crea una raiz con carpetas de prueba y PDFs validos para estresar las reglas de main.rs"
    );
}

fn prepare_output_dir(output_dir: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    Ok(())
}

fn build_folder_plan(folder_index: usize) -> Vec<SampleFile> {
    let total = VALID_NAMES.len();
    let primary = VALID_NAMES[(folder_index - 1) % total];
    let secondary = VALID_NAMES[(folder_index + 6) % total];
    let tertiary = VALID_NAMES[(folder_index + 12) % total];

    let mut samples = Vec::new();
    samples.extend(build_primary_group(primary, folder_index));
    samples.extend(build_single_variant_case(secondary, folder_index));
    samples.extend(build_exact_only_case(tertiary, folder_index));
    samples.extend(build_typo_case(folder_index));
    samples.extend(build_noise_cases(folder_index));

    samples
}

fn build_primary_group(base: &str, folder_index: usize) -> Vec<SampleFile> {
    let mixed_base = if folder_index % 4 == 0 {
        base.to_lowercase()
    } else if folder_index % 5 == 0 {
        to_mixed_case(base)
    } else {
        base.to_string()
    };

    vec![
        sample_file(
            format!("{}.pdf", mixed_base),
            format!("carpeta {} base exacta {}", folder_index, base),
            pages_for(folder_index, base, 1),
        ),
        sample_file(
            format!("{}_01.pdf", base),
            format!("carpeta {} variante 01 {}", folder_index, base),
            pages_for(folder_index, base, 2),
        ),
        sample_file(
            format!("{}_2.PDF", base),
            format!("carpeta {} variante mayuscula {}", folder_index, base),
            pages_for(folder_index, base, 3),
        ),
        sample_file(
            format!("{}_01_extra.PdF", base),
            format!("carpeta {} variante larga {}", folder_index, base),
            pages_for(folder_index, base, 4),
        ),
        sample_file(
            format!("{}13.pdf", base),
            format!("carpeta {} caso no valido {}", folder_index, base),
            1,
        ),
    ]
}

fn build_single_variant_case(base: &str, folder_index: usize) -> Vec<SampleFile> {
    vec![sample_file(
        format!("{}_01.pdf", base),
        format!("carpeta {} unica variante {}", folder_index, base),
        pages_for(folder_index, base, 5),
    )]
}

fn build_exact_only_case(base: &str, folder_index: usize) -> Vec<SampleFile> {
    vec![sample_file(
        format!("{}.pdf", base),
        format!("carpeta {} exacta sola {}", folder_index, base),
        pages_for(folder_index, base, 6),
    )]
}

fn build_typo_case(folder_index: usize) -> Vec<SampleFile> {
    let suffix = if folder_index % 2 == 0 { "01" } else { "extra" };

    vec![
        sample_file(
            "RDH.PdF".to_string(),
            format!("carpeta {} typo rdh exacto", folder_index),
            pages_for(folder_index, "RDH", 7),
        ),
        sample_file(
            format!("RDH_copia_{}.PDF", suffix),
            format!("carpeta {} typo rdh variante {}", folder_index, suffix),
            pages_for(folder_index, "RDH", 8),
        ),
    ]
}

fn build_noise_cases(folder_index: usize) -> Vec<SampleFile> {
    vec![
        sample_file(
            "PI13_noise.pdf".to_string(),
            format!("carpeta {} ruido pi13", folder_index),
            1,
        ),
        sample_file(
            "AES (copia).pdf".to_string(),
            format!("carpeta {} ruido copia", folder_index),
            1,
        ),
        sample_file(
            "CC-1.pdf".to_string(),
            format!("carpeta {} ruido guion", folder_index),
            1,
        ),
        sample_file(
            "119a.pdf".to_string(),
            format!("carpeta {} ruido 119a", folder_index),
            1,
        ),
    ]
}

fn sample_file(file_name: String, title: String, pages: usize) -> SampleFile {
    SampleFile {
        file_name,
        title,
        pages,
    }
}

fn pages_for(folder_index: usize, base: &str, salt: usize) -> usize {
    let seed = folder_index + base.len() + salt;
    1 + (seed % 3)
}

fn to_mixed_case(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for (index, ch) in text.chars().enumerate() {
        if ch.is_ascii_alphabetic() {
            if index % 2 == 0 {
                result.push(ch.to_ascii_lowercase());
            } else {
                result.push(ch.to_ascii_uppercase());
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn write_pdf(path: &Path, title: &str, page_count: usize) -> Result<(), Box<dyn Error>> {
    let bytes = build_pdf_bytes(title, page_count)?;
    fs::write(path, bytes)?;
    Ok(())
}

fn build_pdf_bytes(title: &str, page_count: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    if page_count == 0 {
        return Err("un PDF debe tener al menos una pagina".into());
    }

    let object_count = 2 + page_count * 2 + 1;
    let font_id = object_count;
    let mut objects: Vec<Option<Vec<u8>>> = vec![None; object_count + 1];

    objects[1] = Some(format!("<< /Type /Catalog /Pages 2 0 R >>\n").into_bytes());

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", 3 + index))
        .collect::<Vec<_>>()
        .join(" ");
    objects[2] = Some(
        format!(
            "<< /Type /Pages /Kids [ {} ] /Count {} >>\n",
            kids, page_count
        )
        .into_bytes(),
    );

    for index in 0..page_count {
        let page_id = 3 + index;
        let content_id = 3 + page_count + index;
        let page_object = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>\n",
            font_id, content_id
        );
        objects[page_id] = Some(page_object.into_bytes());

        let text = if page_count == 1 {
            format!("{} | pagina 1 de 1", title)
        } else {
            format!("{} | pagina {} de {}", title, index + 1, page_count)
        };
        let content_stream = build_content_stream(&text);
        let stream = format!(
            "<< /Length {} >>\nstream\n{}endstream\n",
            content_stream.len(),
            content_stream
        );
        objects[content_id] = Some(stream.into_bytes());
    }

    objects[font_id] = Some(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\n"
            .as_bytes()
            .to_vec(),
    );

    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let mut offsets = vec![0usize; object_count + 1];
    for object_id in 1..=object_count {
        offsets[object_id] = buffer.len();
        let object = objects[object_id]
            .take()
            .ok_or_else(|| format!("falta objeto {}", object_id))?;
        buffer.extend_from_slice(format!("{} 0 obj\n", object_id).as_bytes());
        buffer.extend_from_slice(&object);
        buffer.extend_from_slice(b"endobj\n");
    }

    let xref_start = buffer.len();
    buffer.extend_from_slice(format!("xref\n0 {}\n", object_count + 1).as_bytes());
    buffer.extend_from_slice(b"0000000000 65535 f \n");
    for object_id in 1..=object_count {
        buffer.extend_from_slice(format!("{:010} 00000 n \n", offsets[object_id]).as_bytes());
    }
    buffer.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            object_count + 1,
            xref_start
        )
        .as_bytes(),
    );

    Ok(buffer)
}

fn build_content_stream(text: &str) -> String {
    let escaped = escape_pdf_text(text);
    format!("BT\n/F1 18 Tf\n72 780 Td\n({}) Tj\nET\n", escaped)
}

fn escape_pdf_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '(' => escaped.push_str("\\("),
            ')' => escaped.push_str("\\)"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
