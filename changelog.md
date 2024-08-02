# Help system

- Implemented help system. Use !help to get help, !help models to list models, and so on.
  You can also ask it questions in English, but it's not very smart.

# Infrastructure

- The dream command now supports the `-m model` parameter. If you don't specify, it'll try to guess.
- The prompt command now supports `-w width` and `-h height` parameters. These are in pixels, and will override aspect ratio if that is also set. Be careful with this; they will often produce worse results, and usually make the model slower.

# Tips

- Prompt book for anime models: <https://docs.google.com/presentation/d/1HEcE3qOAGVujcDaNQbiLXyx7zwKHQkXEILsYBhsot7A/edit>
- SD3 is out! And needs vastly more descriptive English to work at its best. Dream is more useful than ever, but here's an example: "a female character with long, flowing hair that appears to be made of ethereal, swirling patterns resembling the Northern Lights or Aurora Borealis. The background is dominated by deep blues and purples, creating a mysterious and dramatic atmosphere. The character's face is serene, with pale skin and striking features. She wears a dark-colored outfit with subtle patterns. The overall style of the artwork is reminiscent of fantasy or supernatural genres"

# Models

- Default model changed to Flux1-dev. Use highly descriptive english! Dream works well.
- Added Flux1-dev as `-m flow`. This is a highly capable anime model, if you use highly descriptive english. It also does photorealistic output with `--np`.
- Added AuraFlow v0.1 as `-m auraflow`. This is obviously experimental, so play a bit. It works better with dream than regular prompting, probably.
- Swapped `-m realistic` to RealVisXL 4.0. It's quite good at making pretty things.
- Added `-m pixelart`. This produces... pixel art. Add "16 bit", "32 bit" or "64 bit" to the prompt to control the detail level, and use danbooru tags.
- Added Proteus v0.4 as `-m proteus`. It's a stylized model similar to MidJourney, with extra text support.
- Added countersushi & countersushi-anime. These are partial Stable Cascade finetunes; you should use tags _and_ English. `-m cascade` is now countersushi, but `-m cascade-baseline` is available if you prefer the old behavior.
- Added ConfettiXL. This model is similar to animaginexl, but better at multi-character scenes. Otherwise it's a sort of anime-cartoon hybrid. Not fluffy kitten-safe.
- Anime_XL alias swapped to animaginexl. Tag as per <https://usercontent.irccloud-cdn.com/file/wvPDwUgC/image.png>
- Added the `pixart`, um, "model". This is actually not Stable Diffusion at all. It's an extremely experimental model that should be extremely good at English.
- Try `-m pixart` for anything complicated. It might work, you never know.
- Added ZavyChromaXL v3, which specializes in fantasy realism. Use English. This is the default for magic-themed dreams.
- Added ZavyYumeXL, also aliased as `-m Painterly`. This is an attempted mixture of anime and cartoon style. Try it with --np as well.
- Added Dreamshaper alpha2, also aliased as `-m Realistic`. This is intended for photorealistic outputs. Plus dragons.
- Added duchaitenxl. This is a semi-photorealistic model tuned for aesthetics. Use English descriptions, not tags.
- Added rundiffusionxl. This is a photorealistic model, intended for various forms of fantasy art. Use English, not tags.
- Added breakdomainxl. This is a stylized anime model, but unlike most of them is happy to draw less-pretty things; which will still be drawn well.
- Added realcartoonxl. This is an AOM-style 'photorealistic anime' model.
- Modified `-m animaginexl-realistic` to be more realistic.
