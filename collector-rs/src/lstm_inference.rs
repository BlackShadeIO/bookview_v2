use std::path::Path;

use anyhow::{Context, Result};
use ort::value::Tensor;

const NUM_LAYERS: usize = 2;
const HIDDEN_SIZE: usize = 64;
const NUM_FEATURES: usize = 14;

pub struct LstmPredictor {
    session: ort::session::Session,
    h_state: Vec<f32>,
    c_state: Vec<f32>,
    feature_means: [f32; NUM_FEATURES],
    feature_stds: [f32; NUM_FEATURES],
}

impl LstmPredictor {
    pub fn load(model_path: &Path, stats_path: &Path) -> Result<Self> {
        ort::init().commit().ok();

        let session = ort::session::Session::builder()?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .commit_from_file(model_path)
            .context("failed to load ONNX model")?;

        let stats_text =
            std::fs::read_to_string(stats_path).context("failed to read feature stats")?;
        let stats: serde_json::Value =
            serde_json::from_str(&stats_text).context("invalid feature stats JSON")?;

        let means_arr = stats["means"].as_array().context("missing means")?;
        let stds_arr = stats["stds"].as_array().context("missing stds")?;

        if means_arr.len() != NUM_FEATURES || stds_arr.len() != NUM_FEATURES {
            anyhow::bail!(
                "feature stats have wrong dimensions: {}/{}",
                means_arr.len(),
                stds_arr.len()
            );
        }

        let mut feature_means = [0.0f32; NUM_FEATURES];
        let mut feature_stds = [1.0f32; NUM_FEATURES];
        for i in 0..NUM_FEATURES {
            feature_means[i] = means_arr[i].as_f64().unwrap_or(0.0) as f32;
            feature_stds[i] = stds_arr[i].as_f64().unwrap_or(1.0) as f32;
            if feature_stds[i].abs() < 1e-8 {
                feature_stds[i] = 1.0;
            }
        }

        Ok(Self {
            session,
            h_state: vec![0.0; NUM_LAYERS * HIDDEN_SIZE],
            c_state: vec![0.0; NUM_LAYERS * HIDDEN_SIZE],
            feature_means,
            feature_stds,
        })
    }

    pub fn predict(&mut self, features: &[f32; NUM_FEATURES]) -> f32 {
        let mut normalized = [0.0f32; NUM_FEATURES];
        for i in 0..NUM_FEATURES {
            normalized[i] = (features[i] - self.feature_means[i]) / self.feature_stds[i];
            if !normalized[i].is_finite() {
                normalized[i] = 0.0;
            }
        }

        let input = match Tensor::from_array(([1usize, 1, NUM_FEATURES], normalized.to_vec())) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("LSTM input tensor creation failed: {e}");
                return 0.5;
            }
        };
        let h0 = match Tensor::from_array(([NUM_LAYERS, 1, HIDDEN_SIZE], self.h_state.clone())) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("LSTM h0 tensor creation failed: {e}");
                return 0.5;
            }
        };
        let c0 = match Tensor::from_array(([NUM_LAYERS, 1, HIDDEN_SIZE], self.c_state.clone())) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("LSTM c0 tensor creation failed: {e}");
                return 0.5;
            }
        };

        let outputs = match self.session.run(ort::inputs![
            "features" => input,
            "h0" => h0,
            "c0" => c0,
        ]) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("LSTM inference failed: {e}");
                return 0.5;
            }
        };

        if let Some(hn) = outputs.get("hn") {
            if let Ok((_shape, slice)) = hn.try_extract_tensor::<f32>() {
                if slice.len() == self.h_state.len() {
                    self.h_state.copy_from_slice(slice);
                }
            }
        }
        if let Some(cn) = outputs.get("cn") {
            if let Ok((_shape, slice)) = cn.try_extract_tensor::<f32>() {
                if slice.len() == self.c_state.len() {
                    self.c_state.copy_from_slice(slice);
                }
            }
        }

        let pred: f32 = outputs
            .get("prediction")
            .and_then(|v| v.try_extract_tensor::<f32>().ok())
            .and_then(|(_shape, slice)| slice.first().copied())
            .unwrap_or(0.5);

        pred.clamp(0.0, 1.0)
    }
}

pub fn load_model_if_available(model_dir: &Path) -> Option<LstmPredictor> {
    let model_path = model_dir.join("current.onnx");
    let stats_path = model_dir.join("feature_stats.json");

    if !model_path.exists() || !stats_path.exists() {
        return None;
    }

    match LstmPredictor::load(&model_path, &stats_path) {
        Ok(predictor) => {
            tracing::info!("LSTM model loaded from {}", model_path.display());
            Some(predictor)
        }
        Err(e) => {
            tracing::warn!("Failed to load LSTM model: {e}");
            None
        }
    }
}
