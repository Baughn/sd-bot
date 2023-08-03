# Help system
- Implemented help system. Use !help to get help, !help models to list models, and so on.
  You can also ask it questions in English, but it's not very smart.
- Implemented changelog.

# Models
- SDXL 1.0 is out, and now the default model. 0.9 is still available as `-m sdxl_0.9`.
- Added AstraeaPixie_Radiance v1.6 as `-m AstraeaPixie`.
  This is tuned for high quality anime portraits, but becomes a more vanilla model with --np.
- The first SDXL anime model, https://civitai.com/models/117259/anime-art-diffusion-xl, is out.
  This is now available as `-m anime_art_xl_alpha3` or (currently) `-m Anime_XL`. We also have alpha2.
- Added Anime_XL_Realistic, a preset for Anime_XL that tunes for realistic(-ish) output.
- Added Dreamshaper alpha2, also aliased as `-m Realistic`. This is intended for photorealistic outputs. Plus dragons.
