use colored::Colorize;
use rustyline::config::Configurer;
use rustyline::{DefaultEditor, Result};
use serde_json::json;

use zen_expression::{Isolate, Variable};

trait PrettyPrint {
    fn pretty_print(&self) -> String;
}

impl PrettyPrint for Variable {
    fn pretty_print(&self) -> String {
        match &self {
            Variable::Number(num) => format!("{}", num.to_string().yellow()),
            Variable::String(str) => format!("{}", format!("'{}'", str).green()),
            Variable::Bool(b) => format!("{}", b.to_string().yellow()),
            Variable::Null => format!("{}", "null".bold()),
            Variable::Array(a) => {
                let arr = a.borrow();
                let elements = arr
                    .iter()
                    .map(|i| i.pretty_print())
                    .collect::<Vec<String>>()
                    .join(", ");
                format!("[{}]", elements)
            }
            Variable::Object(m) => {
                let map = m.borrow();
                let elements = map
                    .iter()
                    .map(|(key, value)| format!("{}: {}", key, value.pretty_print()))
                    .collect::<Vec<String>>()
                    .join(", ");

                format!("{{ {} }}", elements)
            }
        }
    }
}

fn main() -> Result<()> {
    let mut rl = DefaultEditor::new()?;
    rl.set_auto_add_history(true);

    loop {
        let readline = rl.readline("> ");
        let Ok(line) = readline else {
            break;
        };

        let mut isolate = Isolate::new();
        isolate.set_environment(
            json!({ "customer": { "firstName": "John", "lastName": "Doe", "age": 20 }, "hello": true, "$": 10 }).into(),
        );
        let result = isolate.run_standard(line.as_str());

        match result {
            Ok(res) => println!("{}", res.pretty_print()),
            Err(err) => println!("Error: {}", err.to_string().red()),
        };
    }

    Ok(())
}
