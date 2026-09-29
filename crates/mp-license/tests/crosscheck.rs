//! 交叉验证：Node 服务端签发的激活码，**必须**能被 Rust 客户端验签通过。
//!
//! 向量由 `node server/src/cli-gen.js vector` 生成，请勿手改。
//!
//! # 为什么需要这个测试
//!
//! 付费用户拿到的激活码由 `server/`（Node）签发、由 `mp-license`（Rust）验签。
//! 两边各自独立实现了一套 payload 拼装 + Crockford Base32 编码，**没有任何编译期约束**
//! 能保证它们一致。一旦某侧改了字段偏移、字节序或编码方式，客户端就会把用户
//! 花钱买来的码判为"签名无效"，且这种故障只会在用户付款后才暴露。
//!
//! 因此这里用固定向量把两侧钉死：**此测试失败 = 禁止发版**。

use mp_license::license::{Edition, Features, License};
use mp_license::PublicKey;

const PUBLIC_KEY_HEX: &str = "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8";
const CODE: &str = "MP1-040PAMZH-00000007-04HMASW9-NF6YY093-8NKRKAYD-XW000000-00000AMS-55TWJ2FW-QMBCX4DB-Q622PJAP-W6MPFFN4-90R7PQA5-628NM1GC-C8PVA3QC-TX6FGSCJ-M5EXJZ14-8G44MHSA-DNDBY0RT-CBYKB7R5-9WY0W";

#[test]
fn node_issued_code_passes_rust_verification() {
    let pk = PublicKey::from_hex(PUBLIC_KEY_HEX).expect("向量公钥解析失败");
    let l = License::decode(CODE).expect("激活码解析失败");

    // 先对字段逐个断言：字段错位往往比验签失败更容易定位
    assert_eq!(l.version, 1);
    assert_eq!(l.edition, Edition::Buyout);
    assert_eq!(l.machine_id, "0123456789abcdef0123456789abcdef");
    assert_eq!(l.serial, 42);
    assert_eq!(l.issued_at, 1_700_000_000);
    assert_eq!(l.features, Features::ALL);

    l.verify_with(&pk)
        .expect("验签失败：Node 与 Rust 的 payload 布局或编码已不一致");
}

#[test]
fn node_issued_code_binds_expected_machine() {
    let l = License::decode(CODE).expect("激活码解析失败");
    l.check_machine("0123456789abcdef0123456789abcdef")
        .expect("应绑定向量指定的机器");
    assert!(l.check_machine("ffffffffffffffffffffffffffffffff").is_err());
}

/// 客户端解析要对粘贴格式宽容：大小写、空格、丢失的横杠都不该让用户卡住
#[test]
fn node_issued_code_tolerates_messy_input() {
    let messy = CODE.to_lowercase().replace('-', " ");
    let l = License::decode(&messy).expect("脏输入也应能解析");
    assert_eq!(l.serial, 42);
}
