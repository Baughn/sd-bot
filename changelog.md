# Help system
- Implemented help system. Use !help to get help, !help models to list models, and so on.
  You can also ask it questions in English, but it's not very smart.

# Infrastructure
- The dream command now supports the `-m model` parameter. If you don't specify, it'll try to guess.

# Tips
- Prompt book: https://docs.google.com/presentation/d/1HEcE3qOAGVujcDaNQbiLXyx7zwKHQkXEILsYBhsot7A/edit

# Models
- Default model swapped to YamerXL. The previous model was `-m BreakDomainXL`.
- Anime_XL alias swapped to KohakuXL. This is a partially-completed full finetune based on Danbooru. Let's give it a try.
- Added the `pixart`, um, "model". This is actually not Stable Diffusion at all. It's an extremely experimental model that should be extremely good at English.
- Try `-m pixart` for anything complicated. It might work, you never know.
- Added ZavyChromaXL v3, which specializes in fantasy realism. Use English. This is the default for magic-themed dreams.
- Added ZavyYumeXL, also aliased as `-m Painterly`. This is an attempted mixture of anime and cartoon style. Try it with --np as well.
- Added Dreamshaper alpha2, also aliased as `-m Realistic`. This is intended for photorealistic outputs. Plus dragons.
- Added duchaitenxl. This is a semi-photorealistic model tuned for aesthetics. Use English descriptions, not tags.
- Added rundiffusionxl. This is a photorealistic model, intended for various forms of fantasy art. Use English, not tags.
- Added breakdomainxl. This is a stylized anime model, but unlike most of them is happy to draw less-pretty things; which will still be drawn well.
- Added realcartoonxl. This is an AOM-style 'photorealistic anime' model.
