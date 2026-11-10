// 运行时配置来自环境变量（设计 §10）：
//   HB_DB_PATH  SQLite 数据库文件路径（默认：./hummingbird.db，相对工作目录）
//   HB_PORT     监听端口（默认：8080）

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: String,
    pub bind_addr: String,
}

impl Config {
    pub fn from_env() -> Self {
        let db_path = std::env::var("HB_DB_PATH").unwrap_or_else(|_| "./hummingbird.db".into());
        let port = std::env::var("HB_PORT").unwrap_or_else(|_| "8080".into());
        Self {
            db_path,
            bind_addr: format!("0.0.0.0:{port}"),
        }
    }
}
