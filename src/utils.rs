use std::{io::Cursor, os::unix::prelude::PermissionsExt};

use anyhow::{bail, Context, Result};
use image::GenericImage;
use log::{debug, info};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

use crate::config::BotConfigModule;

pub fn gallery_geometry(image_count: usize) -> (u32, u32) {
    let width = (image_count as f64).sqrt().ceil() as u32;
    let height = (image_count as f64 / width as f64).ceil() as u32;
    (width, height)
}

/// Given a bunch of PNGs, generates a tiled overview of them.
/// This is used to 'subtly' encourage people to use the upsize buttons.
pub fn overview_of_pictures(pngs: &[Vec<u8>]) -> Result<Vec<u8>> {
    const BORDER: u32 = 8;
    // Parse the JPEGs.
    let images = pngs
        .iter()
        .map(|jpeg| {
            image::load_from_memory(jpeg)
                .context("failed to parse PNG")
                .map(|image| image.to_rgb8())
        })
        .collect::<Result<Vec<_>>>()
        .context("failed to parse PNGs")?;
    if let Some(sample) = images.first() {
        // Decide on a color for the border.
        // We'll use the average color of all the images.
        let mut border_color = [0, 0, 0];
        for image in images.iter() {
            let (width, height) = (image.width(), image.height());
            let mut sum = [0, 0, 0];
            for rgb in image.pixels() {
                sum[0] += rgb.0[0] as u64;
                sum[1] += rgb.0[1] as u64;
                sum[2] += rgb.0[2] as u64;
            }
            let count = width as u64 * height as u64;
            border_color[0] += sum[0] / count;
            border_color[1] += sum[1] / count;
            border_color[2] += sum[2] / count;
        }
        border_color[0] /= images.len() as u64;
        border_color[1] /= images.len() as u64;
        border_color[2] /= images.len() as u64;
        let border_color = image::Rgb([
            border_color[0] as u8,
            border_color[1] as u8,
            border_color[2] as u8,
        ]);
        // Figure out the size of the overview.
        // We'll try to be square-ish.
        let (width_images, height_images) = gallery_geometry(images.len());
        let width = sample.width();
        let height = sample.height();
        // We'll add a border between all images, and around the outside.
        let overview_width = width * width_images + BORDER * (width_images + 1);
        let overview_height = height * height_images + BORDER * (height_images + 1);
        let mut overview = image::RgbImage::new(overview_width, overview_height);
        // Set the background color.
        for pixel in overview.pixels_mut() {
            *pixel = border_color;
        }
        // Copy the images into the overview.
        for (i, image) in images.iter().enumerate() {
            let x = (i as u32 % width_images) * width + BORDER * (i as u32 % width_images + 1);
            let y = (i as u32 / width_images) * height + BORDER * (i as u32 / width_images + 1);
            overview
                .copy_from(image, x, y)
                .context("failed to copy image")?;
        }
        // Encode the overview.
        let mut output = Vec::new();
        overview
            .write_to(
                &mut Cursor::new(&mut output),
                image::ImageOutputFormat::WebP,
            )
            .context("failed to encode WebP")?;
        Ok(output)
    } else {
        bail!("No images");
    }
}

pub async fn upload_images(
    config: &BotConfigModule,
    uuid: &Uuid,
    images: Vec<Vec<u8>>,
) -> Result<Vec<String>> {
    let mut urls = Vec::new();
    // First, we save the images to temporary files.
    let tmp = tempfile::Builder::new()
        .prefix("GANBot")
        .tempdir()
        .context("failed to create temporary directory")?;
    debug!(
        "Uploading {} bytes in {} images",
        images.iter().map(|i| i.len()).sum::<usize>(),
        images.len()
    );
    let temporaries = images
        .into_iter()
        .enumerate()
        .map(|(i, data)| {
            // These should all be jpegs. We'll assume.
            let filename = format!("{}.{}.jpeg", uuid, i);
            let path = tmp.path().join(&filename);
            std::fs::write(&path, data).context("failed to write temporary file")?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .context("failed to chmod temporary file")?;
            anyhow::Ok((filename, path))
        })
        .collect::<Result<Vec<_>>>()
        .context("failed to write temporary files")?;
    // Then upload them all at once, using scp.
    let (host, webdir, relative) = {
        config
            .with_config(|c| {
                (
                    c.backend.webhost.clone(),
                    c.backend.webdir.clone(),
                    c.backend.webdir_internal.clone(),
                )
            })
            .await
    };
    let mut command = tokio::process::Command::new("scp");
    command
        .env_remove("LD_PRELOAD") // SSH doesn't like tcmalloc.
        .arg("-p"); // Preserve access bits.
    for (_, path) in &temporaries {
        command.arg(path);
    }
    command.arg(format!("{host}:{webdir}/{relative}/"));
    debug!("Running {:?}", &command);
    let status = command.status().await.context("failed to run scp")?;
    if !status.success() {
        bail!("scp failed: {}", status);
    }
    for (filename, _) in temporaries {
        urls.push(format!("https://{host}/{relative}/{filename}"));
    }

    Ok(urls)
}

/// Breaks text into paragraphs.
pub fn break_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n").map(|s| s.to_string()).collect()
}

/// Breaks a string into two halves, with the first half being at most `length_limit` bytes long.
fn break_line(text: &str, length_limit: usize) -> (&str, &str) {
    // unicode_word_indices gives us the start of the words, but we want the ends.
    // So we skip the first one, and then add the end of the string.
    let mut boundaries = text
        .unicode_word_indices()
        .map(|(i, _)| i)
        .skip(1)
        .collect::<Vec<_>>();
    boundaries.push(text.len());
    if let Some(first) = boundaries.first() {
        if *first > length_limit {
            // We can't even fit the first word in the line.
            // Just break it at the length limit.
            return (&text[..length_limit], &text[length_limit..]);
        }
        let mut best = *first;
        for boundary in boundaries.iter() {
            if *boundary > length_limit {
                // We've gone too far.
                break;
            }
            best = *boundary;
        }
        (&text[..best], &text[best..])
    } else {
        // Uh, this string is empty.
        ("", "")
    }
}

/// Segment a string (which may be one or more lines) into lines of at most `length_limit` characters.
pub fn segment_lines(text: &str, length_limit: usize) -> Vec<&str> {
    text.lines()
        .flat_map(|line| {
            let mut lines = vec![];
            let mut remaining = line;
            loop {
                let (first, rest) = break_line(remaining, length_limit);
                lines.push(first);
                remaining = rest;
                if remaining.is_empty() {
                    break;
                }
            }
            lines
        })
        .collect()
}

/// Like segment_lines, but tries to return multi-line segments of at most `length_limit` characters.
pub fn segment_lines_condensed(text: &str, length_limit: usize) -> Vec<String> {
    let mut segments = vec![String::new()];
    for line in segment_lines(text, length_limit) {
        if segments.last().unwrap().len() + line.len() > length_limit {
            segments.push(String::new());
        }
        segments.last_mut().unwrap().push_str(line);
        segments.last_mut().unwrap().push('\n');
    }
    segments
}

pub fn hash(text: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    let hash = hasher.finalize();
    hash.to_string()
}

/// Parses a serialized image (JPG, PNG, etc) and converts it to a JPEG.
pub fn convert_to_jpeg(image: Vec<u8>) -> Result<Vec<u8>> {
    let image = image::load_from_memory(&image).context("failed to parse image")?;
    let mut output = Vec::new();
    image
        .write_to(
            &mut Cursor::new(&mut output),
            image::ImageOutputFormat::Jpeg(90),
        )
        .context("failed to encode WebP")?;
    Ok(output)
}

pub fn extract_url(text: &str) -> Option<&str> {
    let mut url = None;
    for word in text.split_whitespace() {
        if word.starts_with("http://") || word.starts_with("https://") {
            url = Some(word);
            break;
        }
    }
    url
}

pub fn get_individual_url(url: &str, replacement: &str) -> Result<String> {
    // This should end in ".0.EXT", and we'll replace the 0.
    // Might be jpg, might be png, might be jpeg.
    if let Some((prefix, suffix)) = url.rsplit_once(".0.") {
        let new_url = format!("{}.{}.{}", prefix, replacement, suffix);
        debug!("Replacing {} with {}", url, new_url);
        Ok(new_url)
    } else {
        bail!("Expected url to end in .0.foo");
    }
}

pub(crate) fn simplify_fraction(width: u32, height: u32) -> (u32, u32) {
    let gcd = num::integer::gcd(width, height);
    (width / gcd, height / gcd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_individual_url() {
        assert_eq!(
            get_individual_url("https://example.com/123.0.jpg", "456").unwrap(),
            "https://example.com/123.456.jpg"
        );
        assert_eq!(
            get_individual_url("https://example.com/123.0.png", "456").unwrap(),
            "https://example.com/123.456.png"
        );
        assert_eq!(
            get_individual_url("https://example.com/123.0.jpeg", "456").unwrap(),
            "https://example.com/123.456.jpeg"
        );
    }

    #[test]
    fn test_extract_url() {
        assert_eq!(extract_url("hello world"), None);
        assert_eq!(
            extract_url("http://example.com not ready"),
            Some("http://example.com")
        );
        assert_eq!(
            extract_url("hello https://example.com world"),
            Some("https://example.com")
        );
    }

    #[test]
    fn test_simplify_fraction() {
        // Test 100 or so random fractions.
        for _ in 0..100 {
            let width = rand::random::<u32>() % 1000;
            let height = rand::random::<u32>() % 1000;
            let (w, h) = simplify_fraction(width, height);
            // Assert the fraction doesn't change.
            assert_eq!(width as f32 / height as f32, w as f32 / h as f32);
        }
    }

    #[test]
    fn test_segment_short() {
        assert_eq!(segment_lines("", 10).len(), 0);
        assert_eq!(segment_lines("hello", 10), vec!["hello"]);
    }

    #[test]
    fn test_segment_long() {
        assert_eq!(segment_lines("hello world", 10), vec!["hello ", "world"]);
    }

    #[test]
    fn test_hash() {
        assert_eq!(
            hash("hello"),
            "ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f"
        );
    }

    #[test]
    fn test_geometry() {
        for image_count in 1..=64 {
            let (width, height) = gallery_geometry(image_count);
            assert!(width * height >= image_count as u32);
            assert!(width + 1 >= height);
            assert!(height + 1 >= width);
        }
    }
}
