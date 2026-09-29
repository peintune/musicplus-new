//! MusicPlus 命令行工具
//!
//! ```text
//! mp convert 音乐目录/ -o ./out -t mp3      # 批量转换
//! mp formats                                # 查看各格式支持状态
//! mp machine-id                             # 显示本机机器码（购买授权需要）
//! mp activate MP1-XXXX-…                    # 离线激活（无需联网）
//! mp status                                 # 查看授权状态
//! ```

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use crossbeam_channel::bounded;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use mp_core::format::TargetFormat;
use mp_core::pipeline::{collect_jobs, ConvertJob};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mp", version, about = "MusicPlus 音乐格式转换")]
struct Cli {
    /// 输出详细日志
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 转换文件或目录
    Convert {
        /// 输入文件或目录（可多个）
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// 输出目录
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 目标格式：keep(原始编码) / mp3 / wav
        #[arg(short, long, default_value = "keep")]
        target: String,
        /// MP3 码率
        #[arg(long, default_value_t = 320)]
        kbps: u32,
        /// 并发数（0=自动）
        #[arg(short, long, default_value_t = 0)]
        jobs: usize,
    },

    /// 列出各输入格式的支持状态
    Formats,

    /// 显示本机机器码
    MachineId,

    /// 离线激活
    Activate {
        /// 激活码（MP1-…）
        code: String,
    },

    /// 查看授权状态与今日免费额度
    Status,

    /// 列出各平台检测到的默认下载目录
    Dirs,

    /// 清除本机授权（换机时用）
    Deactivate,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(format!("musicplus={filter},mp_core={filter},mp_license={filter}"))
        .init();

    match cli.cmd {
        Command::Convert { inputs, output, target, kbps, jobs } => {
            cmd_convert(inputs, output, target, kbps, jobs)
        }
        Command::Formats => cmd_formats(),
        Command::MachineId => cmd_machine_id(),
        Command::Activate { code } => cmd_activate(&code),
        Command::Status => cmd_status(),
        Command::Dirs => cmd_dirs(),
        Command::Deactivate => cmd_deactivate(),
    }
}

fn cmd_convert(
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    target: String,
    kbps: u32,
    jobs: usize,
) -> Result<()> {
    let target = TargetFormat::parse(&target)
        .unwrap_or_else(|| panic!("未知目标格式：{target}（可选 keep / mp3 / wav）"));

    let settings = mp_config::Settings::load();
    let out_dir = output
        .or(settings.output_dir.clone())
        .unwrap_or_else(|| PathBuf::from("./musicplus-out"));

    let mut jobs_list: Vec<ConvertJob> = collect_jobs(&inputs, &out_dir, target);
    for j in jobs_list.iter_mut() {
        j.kbps = kbps;
        j.keep_tags = settings.keep_tags;
    }

    if jobs_list.is_empty() {
        bail!("没有找到可处理的文件");
    }

    println!("待处理 {} 个文件，输出目录：{}", jobs_list.len(), out_dir.display());

    let concurrency = if jobs == 0 { settings.effective_concurrency() } else { jobs };

    let (tx, rx) = bounded::<mp_task::TaskEvent>(1024);
    let handle = std::thread::spawn(move || mp_task::run_batch(jobs_list, concurrency, tx));

    // UI 线程：消费事件渲染进度
    let multi = MultiProgress::new();
    let style = ProgressStyle::with_template("{prefix} [{bar:30.cyan/blue}] {pos:>3}% {msg}")
        .unwrap()
        .progress_chars("=> ");
    let mut bars: Vec<ProgressBar> = Vec::new();

    for ev in rx.iter() {
        use mp_task::TaskEvent::*;
        match ev {
            BatchStarted { total } => {
                for _ in 0..total {
                    bars.push(multi.add(ProgressBar::new(100)));
                }
            }
            Started { index, path } => {
                if let Some(b) = bars.get(index) {
                    b.set_style(style.clone());
                    let name = PathBuf::from(&path)
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or(path);
                    b.set_prefix(name);
                    b.set_position(0);
                }
            }
            Progress { index, percent } => {
                if let Some(b) = bars.get(index) {
                    b.set_position(percent as u64);
                }
            }
            Succeeded { index, outcome } => {
                if let Some(b) = bars.get(index) {
                    b.finish_with_message(format!("→ {}", outcome.output.display()));
                }
            }
            Failed { index, error } => {
                if let Some(b) = bars.get(index) {
                    b.finish_with_message(format!("✗ {error}"));
                }
            }
            Finished { ok, failed } => {
                println!("\n完成：成功 {ok}，失败 {failed}");
                break;
            }
        }
    }

    let _ = handle.join();
    Ok(())
}

fn cmd_formats() -> Result<()> {
    println!("{:<10} {:<22} {}", "格式", "扩展名", "状态");
    println!("{}", "-".repeat(56));

    for (format, available) in mp_core::decrypt::status_table() {
        let status = if available { "已支持" } else { "待移植" };
        println!(
            "{:<10} {:<22} {}",
            format.name(),
            format.extensions().join(", "),
            status
        );
    }

    println!("\n通用音频（由 symphonia 直接解码，无需解密）：");
    for f in [
        mp_core::InputFormat::Flac,
        mp_core::InputFormat::Mp3,
        mp_core::InputFormat::Wav,
        mp_core::InputFormat::Ogg,
        mp_core::InputFormat::Opus,
        mp_core::InputFormat::M4a,
    ] {
        println!("{:<10} {:<22} 已支持", f.name(), f.extensions().join(", "));
    }

    println!("\n输出格式：");
    for t in mp_core::transcode::supported_targets() {
        println!("  {}", t.name());
    }
    if !mp_core::transcode::mp3_supported() {
        println!("  （MP3 输出需 --features mp3 重新编译，依赖系统 libmp3lame）");
    }
    Ok(())
}

fn cmd_machine_id() -> Result<()> {
    println!("{}", mp_license::machine_code()?);
    Ok(())
}

fn cmd_activate(code: &str) -> Result<()> {
    match mp_license::activate(code) {
        Ok(license) => {
            println!("✅ 激活成功（全程离线，未发起任何网络请求）");
            println!("   版本    : {:?}", license.edition);
            println!("   流水号  : {}", license.serial_hex());
            println!("   签发时间: {}", license.issued_at_string());
            Ok(())
        }
        Err(e) => bail!("激活失败：{e}"),
    }
}

fn cmd_status() -> Result<()> {
    let st = mp_license::status();
    println!("机器码  : {}", mp_license::machine_code().unwrap_or_default());
    println!("状态    : {}", st.reason);
    if let Some(l) = st.license {
        println!("版本    : {:?}", l.edition);
        println!("流水号  : {}", l.serial_hex());
        println!("签发时间: {}", l.issued_at_string());
    }

    let q = mp_license::quota::info();
    if q.unlimited {
        println!("额度    : 不限量（已激活）");
    } else {
        println!("额度    : 今日剩余 {} / {} 首（未激活，每日免费 {} 首）", q.remaining, q.limit, q.limit);
    }
    Ok(())
}

/// 列出各平台检测到的默认下载目录，便于排障与支持
fn cmd_dirs() -> Result<()> {
    println!("{:<12} {}", "平台", "检测到的默认目录");
    println!("{}", "-".repeat(70));

    for p in mp_core::scan::Platform::ALL {
        let dir = p.pick_default_dir();
        match dir {
            Some(d) => println!("{:<12} {}", p.label(), d.display()),
            None => println!("{:<12} （未检测到，请手动选择目录）", p.label()),
        }
    }
    Ok(())
}

fn cmd_deactivate() -> Result<()> {
    let store = mp_license::LicenseStore::open()?;
    store.clear()?;
    println!("已清除本机授权");
    Ok(())
}
