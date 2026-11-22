//! Embeds the Picsync icon and version metadata into the Windows executable.
//!
//! Cargo has no native way to attach a Windows resource section, so this script
//! renders the brand mark, writes an `.ico` and a `.rc` script into `OUT_DIR`,
//! and runs the MSVC resource compiler (`rc.exe`) to produce a `.res` that the
//! linker merges into the binary. When `rc.exe` cannot be found the build still
//! succeeds without the icon and prints an explanatory warning.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// The artwork lives with the application so the window icon and the executable
// icon can never drift apart. Only the parts this script needs are used.
#[allow(dead_code)]
#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-env-changed=PICSYNC_RC");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo:warning=Picsync: 未嵌入程序图标（仅 Windows MSVC 目标支持）");
        return;
    }

    let Ok(out_dir) = env::var("OUT_DIR").map(PathBuf::from) else {
        println!("cargo:warning=Picsync: 缺少 OUT_DIR，未嵌入程序图标");
        return;
    };
    if let Err(error) = fs::write(out_dir.join("picsync.ico"), ico_bytes(&icon::SIZES)) {
        println!("cargo:warning=Picsync: 无法写入图标文件：{error}");
        return;
    }
    if let Err(error) = fs::write(out_dir.join("picsync.rc"), resource_script()) {
        println!("cargo:warning=Picsync: 无法写入资源脚本：{error}");
        return;
    }

    let Some(rc_tool) = find_resource_compiler() else {
        println!(
            "cargo:warning=Picsync: 未找到 rc.exe，可执行文件将不带图标（可用 PICSYNC_RC 指定路径）"
        );
        return;
    };

    // Arguments stay relative and the working directory is OUT_DIR, because
    // backslashes in resource-script paths are escape sequences.
    let output = Command::new(&rc_tool)
        .current_dir(&out_dir)
        .arg("/nologo")
        .arg("/c")
        .arg("65001")
        .arg("/fo")
        .arg("picsync.res")
        .arg("picsync.rc")
        .output();
    match output {
        Ok(output) if output.status.success() => {
            println!(
                "cargo:rustc-link-arg-bins={}",
                out_dir.join("picsync.res").display()
            );
        }
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            println!(
                "cargo:warning=Picsync: rc.exe 未能生成资源：{}{}",
                stdout.trim(),
                stderr.trim()
            );
        }
        Err(error) => println!(
            "cargo:warning=Picsync: 无法运行 rc.exe（{}）：{error}",
            rc_tool.display()
        ),
    }
}

/// The `.rc` script: application icon plus Explorer's "Properties" metadata.
/// Written with a UTF-8 BOM and an explicit code page for the Chinese strings.
fn resource_script() -> Vec<u8> {
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_owned());
    let mut numbers = version
        .split('.')
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .collect::<Vec<_>>();
    numbers.resize(4, 0);
    let file_version = format!(
        "{},{},{},{}",
        numbers[0], numbers[1], numbers[2], numbers[3]
    );
    let script = format!(
        "#pragma code_page(65001)\n\
         1 ICON \"picsync.ico\"\n\
         \n\
         1 VERSIONINFO\n\
         FILEVERSION {file_version}\n\
         PRODUCTVERSION {file_version}\n\
         FILEFLAGSMASK 0x3fL\n\
         FILEFLAGS 0x0L\n\
         FILEOS 0x40004L\n\
         FILETYPE 0x1L\n\
         FILESUBTYPE 0x0L\n\
         BEGIN\n\
         \x20   BLOCK \"StringFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       BLOCK \"080404b0\"\n\
         \x20       BEGIN\n\
         \x20           VALUE \"CompanyName\", \"Picsync\"\n\
         \x20           VALUE \"FileDescription\", \"Picsync 图片时光归档\"\n\
         \x20           VALUE \"FileVersion\", \"{version}\"\n\
         \x20           VALUE \"InternalName\", \"picsync\"\n\
         \x20           VALUE \"LegalCopyright\", \"MIT License\"\n\
         \x20           VALUE \"OriginalFilename\", \"picsync.exe\"\n\
         \x20           VALUE \"ProductName\", \"Picsync 图片时光归档\"\n\
         \x20           VALUE \"ProductVersion\", \"{version}\"\n\
         \x20       END\n\
         \x20   END\n\
         \x20   BLOCK \"VarFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       VALUE \"Translation\", 0x0804, 1200\n\
         \x20   END\n\
         END\n"
    );
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    bytes
}

/// Locate the MSVC resource compiler: explicit override, `PATH`, an installed
/// Windows SDK, then a Visual Studio installation.
fn find_resource_compiler() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PICSYNC_RC").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(path) = from_path() {
        return Some(path);
    }
    for key in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"] {
        let Some(root) = env::var_os(key).map(PathBuf::from) else {
            continue;
        };
        let kits_bin = root.join("Windows Kits").join("10").join("bin");
        for version_dir in newest_first(&kits_bin) {
            for arch in ["x64", "x86"] {
                let candidate = version_dir.join(arch).join("rc.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let visual_studio = visual_studio_path()?;
    let msvc_dir = visual_studio.join("VC").join("Tools").join("MSVC");
    for version_dir in newest_first(&msvc_dir) {
        let candidate = version_dir
            .join("bin")
            .join("Hostx64")
            .join("x64")
            .join("rc.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn from_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join("rc.exe"))
        .find(|candidate| candidate.is_file())
}

fn visual_studio_path() -> Option<PathBuf> {
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(root) = env::var_os(key).map(PathBuf::from) else {
            continue;
        };
        let vswhere = root
            .join("Microsoft Visual Studio")
            .join("Installer")
            .join("vswhere.exe");
        if !vswhere.is_file() {
            continue;
        }
        let Ok(output) = Command::new(vswhere)
            .args(["-latest", "-products", "*", "-property", "installationPath"])
            .output()
        else {
            continue;
        };
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Subdirectories ordered newest first, using dotted version names numerically.
fn newest_first(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut versions: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    versions.sort_by(|left, right| {
        version_key(right)
            .cmp(&version_key(left))
            .then_with(|| left.cmp(right))
    });
    versions
}

fn version_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .map(|name| {
            name.to_string_lossy()
                .split('.')
                .filter_map(|part| part.parse::<u64>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Build a Windows `.ico` containing every size as a 32-bit bitmap entry. Bitmap
/// entries are used even for 256 px because they are rendered by every Windows
/// icon consumer, whereas PNG-compressed entries are not.
fn ico_bytes(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&size| (size, bitmap_bytes(size, &icon::render(size))))
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        let dimension = if *size >= 256 { 0 } else { *size as u8 };
        out.push(dimension);
        out.push(dimension);
        out.push(0); // palette size
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        out.extend_from_slice(data);
    }
    out
}

/// A 32-bit bottom-up DIB (BITMAPINFOHEADER + BGRA pixels + AND mask).
fn bitmap_bytes(size: u32, rgba: &[u8]) -> Vec<u8> {
    let width = size as usize;
    let mask_row = width.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + width * width * 4 + mask_row * width);
    out.extend_from_slice(&40u32.to_le_bytes()); // header size
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&((width * 2) as i32).to_le_bytes()); // XOR + AND mask
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((width * width * 4) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // resolution, palette and importance
    for y in (0..width).rev() {
        for x in 0..width {
            let index = (y * width + x) * 4;
            out.extend_from_slice(&[
                rgba[index + 2],
                rgba[index + 1],
                rgba[index],
                rgba[index + 3],
            ]);
        }
    }
    out.resize(out.len() + mask_row * width, 0);
    out
}
