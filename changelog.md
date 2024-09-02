# Help system

- Implemented help system. Use !help to get help, !help models to list models, and so on.
  You can also ask it questions in English, but it's not very smart.

# Infrastructure

- The dream command no longer supports -m, because no models other than flux can deal with the literal novels it's now writing.
- The prompt command now supports `-w width` and `-h height` parameters. These are in pixels, and will override aspect ratio if that is also set. Be careful with this; they will often produce worse results, and usually make the model slower.

# Tips

- Prompt book for anime models: <https://docs.google.com/presentation/d/1HEcE3qOAGVujcDaNQbiLXyx7zwKHQkXEILsYBhsot7A/edit>

# Models

- Default model changed to flux. Use highly descriptive english! Dream works well.
- Renamed flux to flux-anime, and added flux-realistic. If what you want is neither, use just `-m flux`, but remember to pick a style!
- Note that `-m flux` (Flux1-dev) currently does not support negative prompts.
