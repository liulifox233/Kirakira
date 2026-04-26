#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Kag3BootPlan {
    pub exec_storage: Vec<String>,
    pub load_scripts: Vec<String>,
    pub process_scenarios: Vec<String>,
}

impl Kag3BootPlan {
    pub fn is_empty(&self) -> bool {
        self.exec_storage.is_empty()
            && self.load_scripts.is_empty()
            && self.process_scenarios.is_empty()
    }
}

pub fn scan_kag3_boot_plan(source: &str) -> Kag3BootPlan {
    Kag3BootPlan {
        exec_storage: scan_string_call_arguments(source, "Scripts.execStorage"),
        load_scripts: scan_string_call_arguments(source, "KAGLoadScript"),
        process_scenarios: scan_string_call_arguments(source, "kag.process"),
    }
}

fn scan_string_call_arguments(source: &str, callee: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut cursor = 0;

    while let Some(relative_position) = source[cursor..].find(callee) {
        cursor += relative_position + callee.len();
        if let Some(argument) = extract_first_string_argument(&source[cursor..]) {
            arguments.push(argument);
        }
    }

    arguments
}

fn extract_first_string_argument(source: &str) -> Option<String> {
    let source = source.trim_start();
    let source = source.strip_prefix('(')?.trim_start();
    let quote = source.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }

    let mut value = String::new();
    let mut escaped = false;
    for character in source[quote.len_utf8()..].chars() {
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(value);
        } else {
            value.push(character);
        }
    }

    None
}
