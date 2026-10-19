//! Placeholder converter that duplicates a file. Exercises the trait and registry
//! until real converters land.

use std::path::PathBuf;

use crate::{unique_sibling_path, Converter, EngineError, Operation, Options, Result};

pub struct CopyConverter;

impl Converter for CopyConverter {
    fn can_handle(&self, _mime: &str) -> bool {
        true
    }

    fn operations(&self, _mime: &str) -> Vec<Operation> {
        vec![Operation::new("copy", "Duplicate", "Copy")]
    }

    fn run(&self, _op: &Operation, inputs: &[PathBuf], _options: &Options) -> Result<Vec<PathBuf>> {
        inputs
            .iter()
            .map(|input| {
                if !input.is_file() {
                    return Err(EngineError::InputNotFound(input.clone()));
                }
                let out = unique_sibling_path(input, "copy", None);
                std::fs::copy(input, &out)?;
                Ok(out)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_next_to_original() {
        let d = std::env::temp_dir().join(format!("rupantar-copy-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let input = d.join("f.txt");
        std::fs::write(&input, b"hello").unwrap();
        let op = CopyConverter.operations("text/plain").remove(0);
        let out = CopyConverter.run(&op, &[input.clone()], &Options::new()).unwrap();
        assert_eq!(out, vec![d.join("f-copy.txt")]);
        assert_eq!(std::fs::read(&out[0]).unwrap(), b"hello");
        assert!(input.exists());
    }

    #[test]
    fn missing_input_errors() {
        let op = CopyConverter.operations("text/plain").remove(0);
        let err = CopyConverter
            .run(&op, &[PathBuf::from("/definitely/not/here.txt")], &Options::new())
            .unwrap_err();
        assert!(matches!(err, EngineError::InputNotFound(_)));
    }
}
