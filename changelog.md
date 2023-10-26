# Help system
- Implemented help system. Use !help to get help, !help models to list models, and so on.
  You can also ask it questions in English, but it's not very smart.

# Infrastructure
- Initial LoRA support. For now just gives a quality boost.

# Tips
- Prompt book: https://docs.google.com/presentation/d/1HEcE3qOAGVujcDaNQbiLXyx7zwKHQkXEILsYBhsot7A/edit

# Models
- Default model swapped to Kohaku XL beta7. The previous model was `-m zavychromaxl`.
- Anime_XL alias swapped to KohakuXL. This is a partially-completed full finetune based on Danbooru. Let's give it a try.
- Added ZavyChromaXL, which specializes in fantasy realism. Use English; don't try for anime, at least not without --nn.
- Added ZavyYumeXL, also aliased as `-m Painterly`. This is an attempted mixture of anime and cartoon style. Try it with --np as well.
- Added Dreamshaper alpha2, also aliased as `-m Realistic`. This is intended for photorealistic outputs. Plus dragons.
- Added duchaitenxl. This is a semi-photorealistic model tuned for aesthetics. Use English descriptions, not tags.
- Added rundiffusionxl. This is a photorealistic model, intended for various forms of fantasy art. Use English, not tags.
- Added breakdomainxl. This is a stylized anime model, but unlike most of them is happy to draw less-pretty things; which will still be drawn well.
- Added realcartoonxl. This is an AOM-style 'photorealistic anime' model.
