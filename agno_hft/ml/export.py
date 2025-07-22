"""
!‹úh
=========

 ¬ô}„ PyTorch !‹úº TorchScript <
å¿( Rust ¨Î-(
"""

import torch
import torch.jit
import numpy as np
from typing import Dict, Any, Optional, Union, Tuple
from pathlib import Path
import json
import logging
from datetime import datetime
import shutil

logger = logging.getLogger(__name__)


class ModelExporter:
    """
    !‹úh
    
    Ÿý
    1. ú!‹º TorchScript
    2. WIú„!‹
    3. ÝX!‹CxÚ
    4. ¡!‹H,
    """
    
    def __init__(self, export_dir: str = "./exported_models"):
        """
        Args:
            export_dir: úî
        """
        self.export_dir = Path(export_dir)
        self.export_dir.mkdir(parents=True, exist_ok=True)
        
    def export_model(
        self,
        model: torch.nn.Module,
        model_name: str,
        example_input: torch.Tensor,
        metadata: Optional[Dict[str, Any]] = None,
        optimize_for_inference: bool = True,
        verify_export: bool = True
    ) -> Dict[str, str]:
        """
        ú!‹º TorchScript
        
        Args:
            model: ú„!‹
            model_name: !‹1
            example_input: :‹8e(¼ýd	
            metadata: !‹CxÚ
            optimize_for_inference: /&*¨
            verify_export: /&WIú
            
        Returns:
            +úï‘„Wx
        """
        # uúH,„úî
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        version_dir = self.export_dir / model_name / timestamp
        version_dir.mkdir(parents=True, exist_ok=True)
        
        # -n!‹ºU0!
        model.eval()
        
        # úï‘
        script_path = version_dir / "model.pt"
        metadata_path = version_dir / "metadata.json"
        
        try:
            # ( torch.jit.trace ýd!‹
            logger.info(f"Tracing model with input shape: {example_input.shape}")
            traced_model = torch.jit.trace(model, example_input)
            
            # *!‹
            if optimize_for_inference:
                logger.info("Optimizing model for inference...")
                traced_model = torch.jit.optimize_for_inference(traced_model)
                
            # ÝX TorchScript !‹
            logger.info(f"Saving TorchScript model to {script_path}")
            traced_model.save(str(script_path))
            
            # WIú
            if verify_export:
                self._verify_export(model, traced_model, example_input)
                
            # ÝXCxÚ
            export_metadata = self._create_metadata(
                model, 
                model_name, 
                timestamp,
                example_input.shape,
                metadata
            )
            
            with open(metadata_path, 'w') as f:
                json.dump(export_metadata, f, indent=2)
                
            # uú latest &_È¥
            latest_link = self.export_dir / model_name / "latest"
            if latest_link.exists():
                latest_link.unlink()
            latest_link.symlink_to(version_dir.name)
            
            logger.info(f"Model exported successfully to {version_dir}")
            
            return {
                'model_path': str(script_path),
                'metadata_path': str(metadata_path),
                'version': timestamp,
                'export_dir': str(version_dir)
            }
            
        except Exception as e:
            logger.error(f"Failed to export model: {e}")
            # 1W„ú
            if version_dir.exists():
                shutil.rmtree(version_dir)
            raise
            
    def export_ensemble(
        self,
        models: Dict[str, torch.nn.Module],
        ensemble_name: str,
        example_inputs: Dict[str, torch.Tensor],
        metadata: Optional[Dict[str, Any]] = None
    ) -> Dict[str, str]:
        """
        úÆ!‹
        
        Args:
            models: !‹Wx
            ensemble_name: Æ1
            example_inputs: Ï!‹„:‹8e
            metadata: CxÚ
            
        Returns:
            úáo
        """
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        ensemble_dir = self.export_dir / f"{ensemble_name}_ensemble" / timestamp
        ensemble_dir.mkdir(parents=True, exist_ok=True)
        
        exported_models = {}
        
        for model_name, model in models.items():
            model_dir = ensemble_dir / model_name
            model_dir.mkdir(exist_ok=True)
            
            # ú®!‹
            model.eval()
            traced_model = torch.jit.trace(model, example_inputs[model_name])
            
            model_path = model_dir / "model.pt"
            traced_model.save(str(model_path))
            
            exported_models[model_name] = str(model_path)
            
        # ÝXÆCxÚ
        ensemble_metadata = {
            'ensemble_name': ensemble_name,
            'timestamp': timestamp,
            'models': exported_models,
            'metadata': metadata or {}
        }
        
        metadata_path = ensemble_dir / "ensemble_metadata.json"
        with open(metadata_path, 'w') as f:
            json.dump(ensemble_metadata, f, indent=2)
            
        return {
            'ensemble_dir': str(ensemble_dir),
            'metadata_path': str(metadata_path),
            'models': exported_models
        }
        
    def _verify_export(
        self, 
        original_model: torch.nn.Module,
        traced_model: torch.jit.ScriptModule,
        example_input: torch.Tensor,
        tolerance: float = 1e-6
    ):
        """
        WIú„!‹
        
        ÔŸË!‹Œú!‹„8ú
        """
        with torch.no_grad():
            # rÖŸË!‹8ú
            original_output = original_model(example_input)
            
            # rÖú!‹8ú
            traced_output = traced_model(example_input)
            
            # U„8ú^‹
            if isinstance(original_output, dict):
                for key in original_output.keys():
                    if key in traced_output:
                        self._compare_tensors(
                            original_output[key],
                            traced_output[key],
                            tolerance,
                            f"Output[{key}]"
                        )
            else:
                self._compare_tensors(
                    original_output,
                    traced_output,
                    tolerance,
                    "Output"
                )
                
        logger.info("Model export verification passed")
        
    def _compare_tensors(
        self,
        tensor1: torch.Tensor,
        tensor2: torch.Tensor,
        tolerance: float,
        name: str
    ):
        """Ôi5Ï"""
        if not torch.allclose(tensor1, tensor2, atol=tolerance):
            max_diff = torch.max(torch.abs(tensor1 - tensor2)).item()
            raise ValueError(
                f"{name} mismatch. Max difference: {max_diff} > {tolerance}"
            )
            
    def _create_metadata(
        self,
        model: torch.nn.Module,
        model_name: str,
        timestamp: str,
        input_shape: Tuple,
        user_metadata: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        """uú!‹CxÚ"""
        metadata = {
            'model_name': model_name,
            'timestamp': timestamp,
            'export_time': datetime.now().isoformat(),
            'model_info': {
                'architecture': model.__class__.__name__,
                'num_parameters': sum(p.numel() for p in model.parameters()),
                'input_shape': list(input_shape),
            },
            'export_info': {
                'pytorch_version': torch.__version__,
                'cuda_available': torch.cuda.is_available(),
                'device': str(next(model.parameters()).device),
            }
        }
        
        # û (6Ð›„CxÚ
        if user_metadata:
            metadata['user_metadata'] = user_metadata
            
        return metadata
        
    def load_exported_model(
        self,
        model_path: str,
        map_location: Optional[Union[str, torch.device]] = None
    ) -> Tuple[torch.jit.ScriptModule, Dict[str, Any]]:
        """
         	ú„!‹
        
        Args:
            model_path: !‹ï‘
            map_location: -™ 
            
        Returns:
            (!‹, CxÚ)
        """
        model_path = Path(model_path)
        
        # ‚œ/îå~model.pt
        if model_path.is_dir():
            model_file = model_path / "model.pt"
            metadata_file = model_path / "metadata.json"
        else:
            model_file = model_path
            metadata_file = model_path.parent / "metadata.json"
            
        #  	!‹
        logger.info(f"Loading model from {model_file}")
        model = torch.jit.load(str(model_file), map_location=map_location)
        
        #  	CxÚ
        metadata = {}
        if metadata_file.exists():
            with open(metadata_file, 'r') as f:
                metadata = json.load(f)
                
        return model, metadata
        
    def list_exported_models(self, model_name: Optional[str] = None) -> Dict[str, List[str]]:
        """
        úòú„!‹
        
        Args:
            model_name: š!‹1None h:ú@	
            
        Returns:
            !‹H,h
        """
        models = {}
        
        if model_name:
            model_dir = self.export_dir / model_name
            if model_dir.exists():
                versions = [
                    d.name for d in model_dir.iterdir() 
                    if d.is_dir() and d.name != "latest"
                ]
                models[model_name] = sorted(versions, reverse=True)
        else:
            # ú@	!‹
            for model_dir in self.export_dir.iterdir():
                if model_dir.is_dir():
                    versions = [
                        d.name for d in model_dir.iterdir() 
                        if d.is_dir() and d.name != "latest"
                    ]
                    if versions:
                        models[model_dir.name] = sorted(versions, reverse=True)
                        
        return models
        
    def cleanup_old_versions(
        self,
        model_name: str,
        keep_versions: int = 5
    ) -> List[str]:
        """
        
H,
        
        Args:
            model_name: !‹1
            keep_versions: ÝY„H,x
            
        Returns:
            *d„H,h
        """
        model_dir = self.export_dir / model_name
        if not model_dir.exists():
            return []
            
        versions = [
            d for d in model_dir.iterdir() 
            if d.is_dir() and d.name != "latest"
        ]
        
        # 	B“’
        versions.sort(key=lambda x: x.name, reverse=True)
        
        deleted = []
        for version_dir in versions[keep_versions:]:
            logger.info(f"Deleting old version: {version_dir}")
            shutil.rmtree(version_dir)
            deleted.append(version_dir.name)
            
        return deleted
        
    def export_for_rust(
        self,
        model: torch.nn.Module,
        model_name: str,
        example_input: torch.Tensor,
        rust_config: Optional[Dict[str, Any]] = None
    ) -> Dict[str, str]:
        """
        €º Rust ¨*„ú
        
        Args:
            model: !‹
            model_name: 1
            example_input: :‹8e
            rust_config: Rust yšMn
            
        Returns:
            úáo
        """
        # ºÝ!‹( CPU 
Rust ¨8( CPU	
        model = model.cpu()
        example_input = example_input.cpu()
        
        # -n Rust *x
        with torch.jit.optimized_execution(True):
            traced_model = torch.jit.trace(model, example_input)
            
            # ÍP!‹
            traced_model = torch.jit.freeze(traced_model)
            
            # M„*
            traced_model = torch.jit.optimize_for_inference(traced_model)
            
        # ú
        result = self.export_model(
            traced_model,
            f"{model_name}_rust",
            example_input,
            metadata={'rust_config': rust_config or {}},
            optimize_for_inference=False,  # ò“*N†
            verify_export=True
        )
        
        return result