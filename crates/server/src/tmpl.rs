// 模板引擎：自实现 {{var}} 占位符替换（设计 §2 —— 不引 tera/handlebars 的极致路线）。
//
// 语法：
//   {{host}}            —— 用 variables["host"] 替换；未提供时替换为空串
//   文本中的 {{ 与 }} 之外的字符原样保留
// 未提供的变量渲染为空串（静默），调用方若需强校验应在渲染前检查 variables_json。

/// 把模板文本中的 {{key}} 替换为 vars 里对应的值（缺失则替换为空串）
pub fn render(template: &str, vars: &serde_json::Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                out.push_str(vars[key].as_str().unwrap_or(""));
                rest = &after[end + 2..];
            }
            // 没有闭合 }}：剩下的内容原样输出
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_known_vars() {
        let vars = json!({"host": "pi", "status": "ok"});
        assert_eq!(render("{{host}} is {{status}}", &vars), "pi is ok");
    }

    #[test]
    fn missing_var_becomes_empty() {
        assert_eq!(render("[{{missing}}]", &json!({})), "[]");
    }

    #[test]
    fn no_vars_keeps_text() {
        assert_eq!(render("plain text", &json!({})), "plain text");
    }

    #[test]
    fn multiple_occurrences() {
        assert_eq!(render("{{a}}+{{a}}", &json!({"a": "1"})), "1+1");
    }

    #[test]
    fn unclosed_brace_left_as_is() {
        assert_eq!(render("{{a} text", &json!({})), "{{a} text");
    }

    #[test]
    fn trims_key_whitespace() {
        assert_eq!(render("{{  a  }}", &json!({"a": "x"})), "x");
    }

    #[test]
    fn non_string_values_ignored() {
        // 数字等非字符串变量按空串处理（调用方应传字符串）
        assert_eq!(render("{{n}}", &json!({"n": 42})), "");
    }

    #[test]
    fn adjacent_placeholders() {
        assert_eq!(render("{{a}}{{b}}", &json!({"a": "x", "b": "y"})), "xy");
    }
}
