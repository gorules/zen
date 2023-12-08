use colored::Colorize;
use rustyline::config::Configurer;
use rustyline::{DefaultEditor, Result};
use serde_json::{json, Value};

use zen_expression_rewrite::isolate::Isolate;

trait PrettyPrint {
    fn pretty_print(&self) -> String;
}

impl PrettyPrint for Value {
    fn pretty_print(&self) -> String {
        match &self {
            Value::Number(num) => format!("{}", num.to_string().yellow()),
            Value::String(str) => format!("{}", format!("'{}'", str).green()),
            Value::Bool(b) => format!("{}", b.to_string().yellow()),
            Value::Null => format!("{}", "null".bold()),
            Value::Array(arr) => {
                let elements = arr
                    .iter()
                    .map(|i| i.pretty_print())
                    .collect::<Vec<String>>()
                    .join(", ");
                format!("[{}]", elements)
            }
            Value::Object(map) => {
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

        let mut isolate = Isolate::default();
        isolate.inject_env(&json!({
          "customer": {
            "email": "hello@gmail.com",
            "totalSpend": 90,
            "country": "GB"
          },
          "product": {
            "currency": "GBP",
            "price": 190,
            "category": ""
          }
        }));
        isolate.set_reference("customer.totalSpend").unwrap();
        let result = isolate.run_standard(line.as_str());

        match result {
            Ok(res) => println!("{}", res.pretty_print()),
            Err(err) => println!("Error: {}", format!("{:?}", err).red()),
        };
    }

    Ok(())
}
