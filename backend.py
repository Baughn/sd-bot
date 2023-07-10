import json
from typing import List

from io import BytesIO
import base64
import time

import image_generation
from fastapi import FastAPI
from PIL import Image, ExifTags
import uvicorn

app = FastAPI()


@app.get("/txt2img")
async def txt2img(model_name: str, prompt: str, negative_prompt: str, use_pos_default: bool, use_neg_default: bool, use_refiner: bool, guidance_scale: float, steps: int, count: int, seed: int, width: int, height: int):
    try:
        if not seed:
            seed = int(time.time())
        images = await image_generation.Generate(
            model_name=model_name,
            prompt=prompt,
            negative_prompt=negative_prompt,
            use_pos_default=use_pos_default,
            use_neg_default=use_neg_default,
            use_refiner=use_refiner,
            guidance_scale=guidance_scale,
            steps=steps,
            count=count,
            seed=seed,
            width=width,
            height=height,
        )
        b64 = []
        for i, image in enumerate(images):
            # Add EXIF data to the image.
            exif = json.dumps({
                'model_name': model_name,
                'prompt': prompt,
                'negative_prompt': negative_prompt,
                'use_pos_default': use_pos_default,
                'use_neg_default': use_neg_default,
                'use_refiner': use_refiner,
                'guidance_scale': guidance_scale,
                'steps': steps,
                'count': count,
                'seed': seed,
                'width': width,
                'height': height,
                'index': i,
            })
            image.getexif()[ExifTags.Base.UserComment] = b'ASCII\0\0\0' + exif.encode('ascii')
            # Sanitize the prompt for the filename.
            prompt = prompt.replace('/', '_').replace('\\', '_').replace(':', '_').replace('*', '_').replace('?', '_').replace('"', '_').replace('<', '_').replace('>', '_').replace('|', '_')
            filename = f'./output/{int(time.time())}_{i}_{prompt[:100]}.jpg'
            image.save(
                filename,
                quality=90,
                format='jpeg',
                exif=image.getexif().tobytes(),
            )
            # Save for return to requester.
            buffered = BytesIO()
            image.save(
                buffered,
                quality=95,
                format='jpeg',
                exif=image.getexif().tobytes(),
            )
            b64.append(base64.b64encode(buffered.getvalue()).decode('utf-8'))
        return {
            'images': b64,
        }
    except Exception as e:
        return {
            'detail': str(e),
        }


if __name__ == '__main__':
    uvicorn.run('backend:app', port=8000, log_level='debug', reload=True, host='0.0.0.0')
