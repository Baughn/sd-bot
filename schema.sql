CREATE TABLE IF NOT EXISTS Users (
    user TEXT PRIMARY KEY,  -- Format: 'discord:<@mention>' or 'irc:<username>'
    settings JSON  -- User settings stored as JSON
);

CREATE INDEX IF NOT EXISTS Users_user ON Users(user);

CREATE TABLE IF NOT EXISTS Changelog_viewed (
    user TEXT NOT NULL,
    seen TEXT NOT NULL  -- Blake4 hash of seen entries
);

CREATE INDEX IF NOT EXISTS Changelog_viewed_user ON Changelog_viewed(user);

CREATE TABLE IF NOT EXISTS User_stats (
    user TEXT PRIMARY KEY,
    total_batches INTEGER NOT NULL,
    total_private_batches INTEGER NOT NULL,
    FOREIGN KEY (user) REFERENCES Users(user)
);

CREATE TABLE IF NOT EXISTS Batches (
    uuid TEXT PRIMARY KEY,
    original_prompt TEXT,  -- Original prompt used by GPT-4, if applicable
    prompt TEXT NOT NULL,
    style_prompt TEXT NOT NULL,
    settings JSON NOT NULL,  -- Generation settings stored as JSON
    user TEXT NOT NULL,  -- User who generated the batch
    gallery TEXT NOT NULL,  -- URL for the image gallery
    FOREIGN KEY (user) REFERENCES Users(user)
);

CREATE INDEX IF NOT EXISTS Batches_user ON Batches(user);

CREATE TABLE IF NOT EXISTS Images (
    image_id INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_index INTEGER,
    url TEXT NOT NULL,  -- URL for the finished picture
    uuid TEXT,  -- Batch to which the image belongs
    FOREIGN KEY (uuid) REFERENCES Batches(uuid)
);

CREATE TABLE IF NOT EXISTS Votes (
    image_id INTEGER NOT NULL,  -- Image voted on
    user TEXT NOT NULL,  -- User who voted
    vote INTEGER NOT NULL,  -- The vote itself, could be -1 (downvote) or 1 (upvote)
    CHECK (vote IN (-1, 1)),  -- To ensure only -1 or 1 can be used as votes
    FOREIGN KEY (image_id) REFERENCES Images(image_id),
    FOREIGN KEY (user) REFERENCES Users(user),
    UNIQUE(image_id, user)  -- To ensure a user can't vote more than once on an image
);

CREATE INDEX IF NOT EXISTS Votes_user ON Votes(user);