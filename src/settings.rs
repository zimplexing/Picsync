use crate::core::DateBasis;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub recursive: bool,
    pub basis: DateBasis,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            source: PathBuf::new(),
            destination: PathBuf::new(),
            recursive: true,
            basis: DateBasis::TakenFirst,
        }
    }
}

pub fn data_directory() -> Result<PathBuf, String> {
    directories::BaseDirs::new()
        .map(|b| b.data_local_dir().join("Picsync"))
        .ok_or_else(|| "无法确定当前用户的 LocalAppData 目录".to_owned())
}

pub fn load(path: &Path) -> Result<Settings, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let settings: Settings = serde_json::from_slice(&bytes)
                .map_err(|e| format!("配置损坏，已使用默认设置（原文件未删除）：{e}"))?;
            if settings.version != 1 {
                return Err("配置版本不受支持，已使用默认设置（原文件未删除）".to_owned());
            }
            Ok(settings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("无法读取配置，已使用默认设置：{e}")),
    }
}

/// Returns a recovery backup when replacing a corrupt or newer config.
pub fn save(path: &Path, settings: &Settings) -> Result<Option<PathBuf>, String> {
    let parent = path.parent().ok_or("配置缺少父目录")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("无法创建配置目录：{e}"))?;
    let backup = if path.try_exists().map_err(|e| e.to_string())? && load(path).is_err() {
        let original = std::fs::read(path).map_err(|e| format!("无法备份原配置：{e}"))?;
        let mut recovery = tempfile::Builder::new()
            .prefix("settings-recovery-")
            .suffix(".json")
            .tempfile_in(parent)
            .map_err(|e| format!("无法创建配置备份：{e}"))?;
        recovery
            .write_all(&original)
            .and_then(|_| recovery.as_file().sync_all())
            .map_err(|e| format!("无法写入配置备份：{e}"))?;
        let (_, recovery_path) = recovery
            .keep()
            .map_err(|e| format!("无法保留配置备份：{}", e.error))?;
        Some(recovery_path)
    } else {
        None
    };
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| format!("无法编码配置：{e}"))?;
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| format!("无法创建临时配置：{e}"))?;
    temp.write_all(&bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|e| format!("无法写入配置：{e}"))?;
    temp.persist(path)
        .map_err(|e| format!("无法安全保存配置：{}", e.error))?;
    Ok(backup)
}

/// A separate log per run; filesystem paths remain only on this computer.
pub fn write_log(directory: &Path, title: &str, lines: &[String]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory).map_err(|e| format!("无法创建日志目录：{e}"))?;
    let mut file = tempfile::Builder::new()
        .prefix(&format!(
            "picsync-{}-",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
        .suffix(".log")
        .tempfile_in(directory)
        .map_err(|e| format!("无法创建日志：{e}"))?;
    writeln!(file, "{title}").map_err(|e| e.to_string())?;
    for line in lines {
        writeln!(file, "{line}").map_err(|e| e.to_string())?;
    }
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    let (_, path) = file
        .keep()
        .map_err(|e| format!("无法保存日志：{}", e.error))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_and_round_trip_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("配置/settings.json");
        assert!(load(&path).unwrap().recursive);
        let mut settings = Settings {
            source: PathBuf::from("F:\\照片 空格\\来源"),
            destination: PathBuf::from("F:\\归档"),
            basis: DateBasis::Modified,
            ..Default::default()
        };
        save(&path, &settings).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.source, settings.source);
        assert_eq!(loaded.destination, settings.destination);
        assert_eq!(loaded.basis, DateBasis::Modified);
        settings.recursive = false;
        save(&path, &settings).unwrap();
        assert!(!load(&path).unwrap().recursive);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn corrupt_and_future_configs_report_errors_without_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "broken").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::write(&path, r#"{"version":999}"#).unwrap();
        assert!(load(&path).is_err());
    }

    #[test]
    fn explicit_save_backs_up_corrupt_or_newer_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for original in [b"corrupt".as_slice(), br#"{"version":999}"#.as_slice()] {
            std::fs::write(&path, original).unwrap();
            let backup = save(&path, &Settings::default()).unwrap().unwrap();
            assert_eq!(std::fs::read(backup).unwrap(), original);
            assert_eq!(load(&path).unwrap().version, 1);
        }
    }

    #[test]
    fn save_failure_does_not_replace_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::create_dir(&path).unwrap();
        assert!(save(&path, &Settings::default()).is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn log_is_retained_and_contains_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_log(dir.path(), "扫描完成", &["来源：照片.jpg".into()]).unwrap();
        assert!(std::fs::read_to_string(path).unwrap().contains("照片.jpg"));
    }
}
