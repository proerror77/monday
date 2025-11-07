use anyhow::Result;
use tracing::info;
use tract_onnx::prelude::*;

pub struct OnnxPredictor {
    model: SimplePlan<TypedFact, Box<dyn TypedOp>, TypedModel>,
    input_shape: (usize, usize, usize, usize), // (N,C,L,K)
}

impl OnnxPredictor {
    pub fn load(onnx_path: &str, input_shape: (usize, usize, usize, usize)) -> Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_path(onnx_path)?
            .into_optimized()? // fold, fuse
            .into_runnable()?; // make it runnable
        info!("ONNX model loaded: {}", onnx_path);
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
