import asyncio
from typing import List, Any

from diffusers import StableDiffusionPipeline, StableDiffusionXLPipeline, StableDiffusionXLImg2ImgPipeline
import torch
import json
from PIL.Image import Image

# Configuration
MODEL_CONFIG = '/home/svein/AI/image-generation/models/config.json'
TARGET_PIXELS_AT_A_TIME = 1024*1024*3

# Example config:
# {
#   "sdxl_0.9": {
#     "pipeline": "SDXLPipeline",
#     "base": "stabilityai/stable-diffusion-xl-base-0.9",
#     "refiner": "stabilityai/stable-diffusion-xl-refiner-0.9"
#   }
# }


# Global variables
loaded_pipeline = None
loaded_pipeline_name = None
generator_lock = asyncio.Lock()


async def Generate(model_name: str = 'default', **kwargs) -> List[Image]:
    # First, (re)load the model file.
    config = json.load(open(MODEL_CONFIG))
    if model_name not in config:
        raise ValueError(f"Model {model_name} not found in config file.")
    if model_name == 'default':
        model_name = config['default']
    model_config = config[model_name]
    global loaded_pipeline
    global loaded_pipeline_name
    if loaded_pipeline_name != model_config['pipeline']:
        loaded_pipeline = eval(model_config['pipeline'])()
        loaded_pipeline_name = model_config['pipeline']
    # Then, generate the images.
    async with generator_lock:
        return loaded_pipeline.generate(config=model_config, **kwargs)


class SDXLPipeline:
    def __init__(self):
        self.base = None
        self.refiner = None

    def generate(self, config: dict[str, Any], use_pos_default: bool, use_neg_default: bool, width: int, height: int, use_refiner: bool, prompt: str, negative_prompt: str, guidance_scale: float, steps: int, count: int, seed: int) -> List[Image]:
        # Reload the models if necessary.
        if self.base != config['base']:
            self.base = StableDiffusionXLPipeline.from_pretrained(config['base'], torch_dtype=torch.float16, variant='fp16', use_safetensors=True)
            self.base.enable_vae_slicing()
            self.base.enable_model_cpu_offload()
            #self.base.unet = torch.compile(self.base.unet, mode='reduce-overhead')
            #torch.cuda.empty_cache()
        if use_refiner and self.refiner != config['refiner']:
            self.refiner = StableDiffusionXLImg2ImgPipeline.from_pretrained(config['refiner'], torch_dtype=torch.float16, variant='fp16', use_safetensors=True)
            self.refiner.enable_vae_slicing()
            self.refiner.enable_model_cpu_offload()
            #self.refiner.unet = torch.compile(self.refiner.unet, mode='reduce-overhead')
            #torch.cuda.empty_cache()
        # Configure.
        if use_pos_default and config['default_positive']:
            prompt = f'{prompt}, {config["default_positive"]}'
        if use_neg_default and config['default_negative']:
            negative_prompt = f'{negative_prompt}, {config["default_negative"]}'
        # Generate the images.
        total_pixels = width*height*count
        batch_size = int(count / max(1, total_pixels / TARGET_PIXELS_AT_A_TIME))
        assert batch_size > 0, "Batch size must be positive."
        print(f'Generating {count} images with seed {seed} and prompt "{prompt}"')
        print(f'Batch size: {batch_size}')
        images = []
        torch.manual_seed(seed)
        while count > 0:
            this_batch_size = min(count, batch_size)
            count -= this_batch_size
            if use_refiner:
                batch = self.base(prompt=prompt, negative_prompt=negative_prompt, guidance_scale=guidance_scale, num_inference_steps=steps, output_type='latent', num_images_per_prompt=this_batch_size, width=width, height=height)[0]
                batch = self.refiner(prompt=prompt, negative_prompt=negative_prompt, image=batch, guidance_scale=guidance_scale, num_inference_steps=int(steps*1.5), output_type='pil', num_images_per_prompt=this_batch_size)[0]
            else:
                batch = self.base(prompt=prompt, negative_prompt=negative_prompt, guidance_scale=guidance_scale, num_inference_steps=steps, output_type='pil', num_images_per_prompt=this_batch_size, width=width, height=height)[0]
            images.extend(batch)
        return images
