// 认证原语：argon2id（按设计 §2 调参 m=8MiB/t=2/p=1——默认 19MiB 参数
// 每次登录瞬时吃 20MB+ 内存）、token 生成与哈希、内存态防爆破锁定
//（失败 5 次 -> 锁 15 分钟，按用户名与 IP 双维度计数）。

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const ARGON2_M_KIB: u32 = 8 * 1024; // 8 MiB
const ARGON2_T: u32 = 2;
const ARGON2_P: u32 = 1;

const LOCK_THRESHOLD: u32 = 5;
const LOCK_WINDOW: Duration = Duration::from_secs(15 * 60);

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    let params = argon2::Params::new(ARGON2_M_KIB, ARGON2_T, ARGON2_P, None).expect("argon2 params");
    let argon = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    argon
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hash")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else { return false };
    let params = argon2::Params::new(ARGON2_M_KIB, ARGON2_T, ARGON2_P, None).expect("argon2 params");
    let argon = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    argon.verify_password(password.as_bytes(), &parsed).is_ok()
}

/// 随机字母数字串（密码、会话 id 用；字符表去掉了易混淆的 0O1lI）。
pub fn gen_random_string(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len()) as usize] as char)
        .collect()
}

/// 任务 token："hb" + 32 字节随机数的十六进制（192 比特熵）。
pub fn gen_task_token() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen::<u8>()).collect();
    format!("hb{}", to_hex(&bytes))
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    to_hex(&hasher.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 防爆破锁定：按 `username` 和 `ip` 双维度计数（设计 §8）——
/// 攻击者既不能只靠用户名把受害者锁死，也不能换 IP 绕开单名计数。
#[derive(Default)]
pub struct Lockout {
    failures: Mutex<HashMap<String, (u32, Option<Instant>)>>,
}

impl Lockout {
    pub fn is_locked(&self, username: &str, ip: &str) -> bool {
        self.locked_at(&format!("u:{username}")) || self.locked_at(&format!("i:{ip}"))
    }

    fn locked_at(&self, key: &str) -> bool {
        let map = self.failures.lock().unwrap();
        map.get(key)
            .and_then(|(_, until)| *until)
            .is_some_and(|u| Instant::now() < u)
    }

    pub fn record_failure(&self, username: &str, ip: &str) {
        let mut map = self.failures.lock().unwrap();
        for key in [format!("u:{username}"), format!("i:{ip}")] {
            let entry = map.entry(key).or_insert((0, None));
            entry.0 += 1;
            if entry.0 >= LOCK_THRESHOLD {
                *entry = (0, Some(Instant::now() + LOCK_WINDOW));
                log::warn!("login locked for 15min: {username} from {ip}");
            }
        }
    }

    pub fn reset(&self, username: &str, ip: &str) {
        let mut map = self.failures.lock().unwrap();
        map.remove(&format!("u:{username}"));
        map.remove(&format!("i:{ip}"));
    }
}
