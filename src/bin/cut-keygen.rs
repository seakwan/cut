//! 离线激活码生成工具。
//!
//! 用法：
//! ```sh
//! cargo run --bin cut-keygen -- <machine_id>              # 永久激活
//! cargo run --bin cut-keygen -- <machine_id> --days 365   # 365 天后过期
//! ```

use cut::license::generate_license_key;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("用法：cut-keygen <机器码> [--days N]");
        eprintln!("示例：");
        eprintln!("  cut-keygen a1b2c3d4e5f67890              # 永久激活");
        eprintln!("  cut-keygen a1b2c3d4e5f67890 --days 365   # 365 天后过期");
        std::process::exit(1);
    }

    let fingerprint = &args[1];
    if fingerprint.len() != 16 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
        eprintln!("错误：机器码应为 16 位 hex 字符串（如 a1b2c3d4e5f67890）");
        std::process::exit(1);
    }

    let mut expires_days: Option<u16> = None;

    let mut i = 2;
    while i < args.len() {
        if args[i] == "--days" {
            i += 1;
            if i >= args.len() {
                eprintln!("错误：--days 后需要跟天数");
                std::process::exit(1);
            }
            expires_days = Some(args[i].parse().unwrap_or_else(|_| {
                eprintln!("错误：天数应为整数");
                std::process::exit(1);
            }));
        } else {
            eprintln!("错误：未知参数 {}", args[i]);
            std::process::exit(1);
        }
        i += 1;
    }

    let key = generate_license_key(fingerprint, expires_days);

    println!("机器码：{}-{}", &fingerprint[..8], &fingerprint[8..]);
    if let Some(days) = expires_days {
        println!("有效期：{days} 天");
    } else {
        println!("有效期：永久");
    }
    println!();
    println!("激活码：");
    println!("{key}");
}
