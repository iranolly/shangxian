//! 日志文件轮转工具：隧道日志、服务日志、app.log 共用（D1 §4）。
//!
//! 轮转规则：`x` → `x.1` → `x.2`，最老的删掉；调用方必须保证当前没有别的
//! 进程/线程在写这个文件（本机服务日志只在 spawn 前调，那时服务不在跑）。
//!
//! 为什么不「运行中实时轮转」：Windows 上把被打开的文件改名是能成功的，但已经
//! 打开的写句柄仍指向同一个文件对象（改名后的那份），新输出继续写进旧文件——
//! 轮转等于没做；`set_len(0)` 截断也不行，子进程的文件位置不会被重置，会写出
//! 稀疏空洞文件。所以服务的运行中超限只做「标记 + 提示 + 手动入口」（D1 §4.1）。

use std::path::{Path, PathBuf};

/// 本机服务日志上限（与隧道日志对齐：5MB）。
pub const SERVICE_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// 本机服务日志保留的备份份数（x.log、x.log.1、x.log.2）。
pub const SERVICE_LOG_BACKUPS: usize = 2;

/// 文件大小；不存在（或读不到元数据）返回 0。
pub fn size_of(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// 超过 max_bytes 就轮转：x → x.1 → x.2，最老的删掉。
/// 调用方必须保证当前没有别的进程/线程在写这个文件（服务日志只在 spawn 前调）。
/// 返回是否真的轮转过。
pub fn rotate_if_needed(path: &Path, max_bytes: u64, backups: usize) -> std::io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if size_of(path) < max_bytes {
        return Ok(false);
    }
    if backups == 0 {
        // 不留备份：直接删掉当前文件，下一次写入会新建。
        std::fs::remove_file(path)?;
        return Ok(true);
    }
    // 最老的备份删掉，旧的依次后移，当前文件改成 .1。
    let _ = std::fs::remove_file(backup_path(path, backups));
    for index in (1..backups).rev() {
        let from = backup_path(path, index);
        if from.exists() {
            let _ = std::fs::rename(&from, backup_path(path, index + 1));
        }
    }
    std::fs::rename(path, backup_path(path, 1))?;
    Ok(true)
}

/// 备份文件路径：给原路径加 `.1`、`.2` 这种后缀。
fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sx-logfile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rotate_noop_below_limit() {
        let dir = temp_dir("noop");
        let path = dir.join("x.log");
        std::fs::write(&path, b"0123456789").unwrap(); // 正好 10 字节
        assert!(!rotate_if_needed(&path, 11, 2).unwrap(), "没超上限不该动它");
        assert!(path.exists());
        assert!(!dir.join("x.log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_at_exact_limit() {
        // 正好等于上限也要轮转（和 D1「app.log ≥ 2MB → 轮转」一致）
        let dir = temp_dir("exact");
        let path = dir.join("x.log");
        std::fs::write(&path, b"0123456789").unwrap();
        assert!(rotate_if_needed(&path, 10, 2).unwrap());
        assert!(!path.exists(), "超上限的当前文件应该被移走");
        assert!(dir.join("x.log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_moves_and_trims() {
        let dir = temp_dir("rotate");
        let path = dir.join("x.log");
        let content = b"0123456789a"; // 11 字节 > 上限 10

        std::fs::write(&path, content).unwrap();
        assert!(rotate_if_needed(&path, 10, 2).unwrap());
        assert!(!path.exists());
        assert!(dir.join("x.log.1").exists());

        std::fs::write(&path, content).unwrap();
        assert!(rotate_if_needed(&path, 10, 2).unwrap());
        assert!(dir.join("x.log.2").exists());

        std::fs::write(&path, content).unwrap();
        assert!(rotate_if_needed(&path, 10, 2).unwrap());
        assert!(dir.join("x.log.2").exists(), "只保留 2 份备份");
        assert!(!dir.join("x.log.3").exists(), "不能留第 3 份");
        assert_eq!(std::fs::read(dir.join("x.log.1")).unwrap(), content);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_without_backups_deletes_current() {
        let dir = temp_dir("zero");
        let path = dir.join("x.log");
        std::fs::write(&path, b"0123456789a").unwrap();
        assert!(rotate_if_needed(&path, 10, 0).unwrap());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn size_of_missing_is_zero() {
        let path = std::env::temp_dir().join(format!(
            "sx-logfile-missing-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        assert_eq!(size_of(&path), 0);
    }
}
