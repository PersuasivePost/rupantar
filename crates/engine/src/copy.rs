//! Placeholder converter that duplicates a file. Exercises the trait and registry
//! until more converters land.

use std::fs::File;
use std::path::PathBuf;

use crate::{catalog, create_unique_sibling, Converter, EngineError, Operation, Options, Result};

pub struct CopyConverter;

impl Converter for CopyConverter {
    fn can_handle(&self, _mime: &str) -> bool {
        true
    }

    fn operations(&self, _mime: &str) -> Vec<Operation> {
        vec![catalog::op("copy")]
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], _options: &Options) -> Result<Vec<PathBuf>> {
        if op.id != "copy" {
            return Err(EngineError::UnknownOperation(op.id.clone()));
        }
        inputs
            .iter()
            .map(|input| {
                if !input.is_file() {
                    return Err(EngineError::InputNotFound(input.clone()));
                }
                let (mut out, path) = create_unique_sibling(input, "copy", None)?;
                std::io::copy(&mut File::open(input)?, &mut out)?;
                Ok(path)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_op() -> Operation {
        CopyConverter.operations("text/plain").remove(0)
    }

    #[test]
    fn copies_next_to_original() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("f.txt");
        std::fs::write(&input, b"hello").unwrap();
        let out = CopyConverter.run(&copy_op(), std::slice::from_ref(&input), &Options::new()).unwrap();
        assert_eq!(out, vec![d.path().join("f (copy).txt")]);
        assert_eq!(std::fs::read(&out[0]).unwrap(), b"hello");
        assert!(input.exists());
    }

    #[test]
    fn missing_input_errors() {
        let err = CopyConverter
            .run(&copy_op(), &[PathBuf::from("/definitely/not/here.txt")], &Options::new())
            .unwrap_err();
        assert!(matches!(err, EngineError::InputNotFound(_)));
    }

    #[test]
    fn foreign_op_is_unknown() {
        let err = CopyConverter.run(&Operation::new("x", "X", "X"), &[], &Options::new()).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOperation(_)));
    }
}
