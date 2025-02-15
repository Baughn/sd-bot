## Help system

- Use !help to get help, !help models to list models, and so on.
  You can also ask it questions in English, but it's not very smart.

## Infrastructure

- The dream command no longer supports -m, because no models other than flux can deal with the literal novels it's now writing.
- The prompt command now supports `-w width` and `-h height` parameters. These are in pixels, and will override aspect ratio if that is also set. Be careful with this; they will often produce worse results, and usually make the model slower.
- Bumped the base resolution for the fanart models. Let me know if this causes an increase in broken anatomy.

## Tips

- Prompt book for anime models: <https://docs.google.com/presentation/d/1HEcE3qOAGVujcDaNQbiLXyx7zwKHQkXEILsYBhsot7A/edit>

## Models

- Flux-realistic model swapped out for Jibmix. This is now the default for !prompt and !dream.
- Swapped the fanart default to `-m ntrmix`. It's pretty much like PASanctuary, but a little better at action scenes.
- Added `--no bored, simple background, monochrome` to the default prompt for fanart-cute, but you can do this with anything~
- Added fanart-cute (`-m fanart-cute`, aka. `-m monody`) as an alternative to `fanart`. This does what it says on the tin.
- Added PASanctuary SDXL as `-m pasanctuary` and `-m fanart`. This model is useful for all your anime fanart purposes. Prompt with tags.
- Note that `-m flux` (Flux1-dev) currently does not support negative prompts.
