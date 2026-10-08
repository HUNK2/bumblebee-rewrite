//! Lists the events of sound event files: what a `.fev` holds, by category.
//!
//!   cargo run -p tf2-core --example fev_list -- <file.fev>... [word]
//!
//! One line a file (project, banks, events, definitions) and the count of events in each
//! category; with a further `word`, every event whose name or category holds it (any case),
//! with its parameters.

use tf2_core::formats::fev::Fev;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (files, words): (Vec<_>, Vec<_>) = args.iter().partition(|a| a.to_lowercase().ends_with(".fev"));
    let word = words.first().map(|w| w.to_lowercase());
    for path in files {
        let fev = match std::fs::read(path).map_err(|e| e.to_string()).and_then(|data| Fev::parse(&data)) {
            Ok(fev) => fev,
            Err(e) => {
                println!("{path}: {e}");
                continue;
            }
        };
        println!("{path}: project {}, banks {:?}, {} events, {} definitions", fev.project, fev.banks, fev.events.len(), fev.definitions.len());
        match &word {
            None => {
                let mut categories: Vec<(&str, usize, &str)> = Vec::new();
                for event in &fev.events {
                    match categories.iter_mut().find(|c| c.0 == event.category) {
                        Some(category) => category.1 += 1,
                        None => categories.push((&event.category, 1, &event.name)),
                    }
                }
                for (category, count, first) in categories {
                    println!("  {count:5}  {category}  (first: {first})");
                }
            }
            Some(word) => {
                for event in fev.events.iter().filter(|e| e.name.to_lowercase().contains(word) || e.category.to_lowercase().contains(word)) {
                    let parameters: Vec<String> = event.parameters.iter().map(|p| format!("{} {}..{}", p.name, p.min, p.max)).collect();
                    println!("  {}  [{}]  {}", event.name, event.category, parameters.join(", "));
                }
            }
        }
    }
}
