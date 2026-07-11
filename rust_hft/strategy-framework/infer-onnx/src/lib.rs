use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};
use tracing::info;
use tract_onnx::prelude::*;

pub const MAX_ONNX_MODEL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ONNX_GRAPH_NODES: usize = 10_000;
pub const MAX_ONNX_INPUT_ELEMENTS: usize = 4 * 1024 * 1024;

pub struct OnnxPredictor {
    model: SimplePlan<TypedFact, Box<dyn TypedOp>, TypedModel>,
    input_shape: (usize, usize, usize, usize), // (N,C,L,K)
}

impl OnnxPredictor {
    pub fn load(onnx_path: &str, input_shape: (usize, usize, usize, usize)) -> Result<Self> {
        let bytes = read_model_bytes(onnx_path)?;
        Self::load_from_bytes(&bytes, onnx_path, input_shape)
    }

    pub fn load_verified(
        onnx_path: &Path,
        expected_sha256: &str,
        input_shape: (usize, usize, usize, usize),
    ) -> Result<Self> {
        anyhow::ensure!(
            expected_sha256.len() == 64
                && expected_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "expected ONNX checksum must be a lowercase SHA-256 value"
        );
        let bytes = read_model_bytes(onnx_path)?;
        let actual_sha256 = hex::encode(Sha256::digest(&bytes));
        anyhow::ensure!(
            actual_sha256 == expected_sha256,
            "ONNX model checksum mismatch"
        );
        Self::load_from_bytes(&bytes, &onnx_path.display().to_string(), input_shape)
    }

    pub fn load_from_bytes(
        bytes: &[u8],
        source: &str,
        input_shape: (usize, usize, usize, usize),
    ) -> Result<Self> {
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_ONNX_MODEL_BYTES,
            "ONNX model exceeds the {} byte limit",
            MAX_ONNX_MODEL_BYTES
        );
        let input_elements = [input_shape.0, input_shape.1, input_shape.2, input_shape.3]
            .into_iter()
            .try_fold(1_usize, usize::checked_mul)
            .context("ONNX input shape overflows usize")?;
        anyhow::ensure!(
            input_elements > 0 && input_elements <= MAX_ONNX_INPUT_ELEMENTS,
            "ONNX input shape exceeds the {} element limit",
            MAX_ONNX_INPUT_ELEMENTS
        );
        let mut cursor = Cursor::new(bytes);
        let model = tract_onnx::onnx().model_for_read(&mut cursor)?;
        anyhow::ensure!(
            model.nodes().len() <= MAX_ONNX_GRAPH_NODES,
            "ONNX graph exceeds the {} node limit",
            MAX_ONNX_GRAPH_NODES
        );
        let model = model.into_optimized()?.into_runnable()?;
        info!("ONNX model loaded: {}", source);
        Ok(Self { model, input_shape })
    }

    pub fn infer(&self, data: &[f32]) -> Result<Vec<f32>> {
        let (n, c, l, k) = self.input_shape;
        let expect = n * c * l * k;
        anyhow::ensure!(
            data.len() == expect,
            "input length mismatch: got {}, expect {}",
            data.len(),
            expect
        );
        let arr = tract_ndarray::Array4::from_shape_vec((n, c, l, k), data.to_vec())?;
        let input: Tensor = arr.into_tensor();
        let outputs = self.model.run(tvec!(input.into()))?;
        let out = outputs[0].to_array_view::<f32>()?.iter().copied().collect();
        Ok(out)
    }
}

fn read_model_bytes(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("failed to open ONNX model {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect ONNX model {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "ONNX model must be a regular file");
    anyhow::ensure!(
        metadata.len() <= MAX_ONNX_MODEL_BYTES,
        "ONNX model exceeds the {} byte limit",
        MAX_ONNX_MODEL_BYTES
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_ONNX_MODEL_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read ONNX model {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_ONNX_MODEL_BYTES,
        "ONNX model exceeds the {} byte limit",
        MAX_ONNX_MODEL_BYTES
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_model(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "hft-infer-onnx-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn verified_load_rejects_bytes_that_do_not_match_the_pinned_digest() {
        let path = temp_model("checksum");
        std::fs::write(&path, b"not the signed model").unwrap();

        let error = OnnxPredictor::load_verified(&path, &"a".repeat(64), (1, 4, 2, 2))
            .err()
            .unwrap();
        std::fs::remove_file(path).unwrap();

        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn model_reads_are_bounded_before_allocation_or_parsing() {
        let path = temp_model("oversized");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"onnx").unwrap();
        file.set_len(MAX_ONNX_MODEL_BYTES + 1).unwrap();

        let error = OnnxPredictor::load(path.to_str().unwrap(), (1, 4, 2, 2))
            .err()
            .unwrap();
        std::fs::remove_file(path).unwrap();

        assert!(error.to_string().contains("byte limit"));
    }

    #[test]
    fn input_shape_is_bounded_before_model_parsing() {
        let error = OnnxPredictor::load_from_bytes(
            b"not an onnx model",
            "memory",
            (1, 4, MAX_ONNX_INPUT_ELEMENTS, 2),
        )
        .err()
        .unwrap();

        assert!(error.to_string().contains("element limit"));
    }
}
