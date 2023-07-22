use std::{os::unix::prelude::PermissionsExt, io::{Write, Cursor}};

use anyhow::{bail, Result, Context};
use image::GenericImage;
use log::{info, debug};
use unicode_segmentation::UnicodeSegmentation;

use crate::config::BotConfigModule;

/// Given a bunch of JPEGs, generates a tiled overview of them.
/// This is used to 'subtly' encourage people to use the upsize buttons.
pub fn overview_of_pictures(jpegs: &[Vec<u8>]) -> Result<Vec<u8>> {
    const BORDER: u32 = 8;
    // Parse the JPEGs.
    let images = jpegs.iter().map(|jpeg| {
        image::load_from_memory(jpeg).context("failed to parse JPEG")
        .map(|image| image.to_rgb8())
    }).collect::<Result<Vec<_>>>()
        .context("failed to parse JPEGs")?;
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
        let border_color = image::Rgb([border_color[0] as u8, border_color[1] as u8, border_color[2] as u8]);
        // Figure out the size of the overview.
        // We'll try to be square-ish.
        let width_images = (images.len() as f64).sqrt().ceil() as u32;
        let height_images = (images.len() as f64 / width_images as f64).ceil() as u32;
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
            overview.copy_from(image, x, y).context("failed to copy image")?;
        }
        // Encode the overview as JPEG.
        let mut output = Vec::new();
        overview.write_to(&mut Cursor::new(&mut output), image::ImageOutputFormat::Jpeg(90))
            .context("failed to encode JPEG")?;
        Ok(output)
    } else {
        bail!("No images");
    }
}

pub async fn upload_images(config: &BotConfigModule, images: Vec<Vec<u8>>) -> Result<Vec<String>> {
    let uuid = uuid::Uuid::new_v4();
    let mut urls = Vec::new();
    for (i, data) in images.iter().enumerate() {
        let filename = format!("{}.{}.jpg", uuid, i);
        info!("Uploading {} bytes to {}", data.len(), filename);
        // Save the image to a temporary file.
        let tmp = tempfile::NamedTempFile::new().context("failed to create temporary file")?;
        tmp.as_file().write_all(data).context("failed to write temporary file")?;
        tmp.as_file().set_permissions(PermissionsExt::from_mode(0o644)).context("failed to chmod temporary file")?;
        // We'll just call scp directly. It's not like we're going to be uploading a lot of images.
        let (host, webdir, relative) = {
            config.with_config(|c| {
                (c.backend.webhost.clone(), c.backend.webdir.clone(), c.backend.webdir_internal.clone())
            }).await
        };
        let mut command = tokio::process::Command::new("scp");
        command
            .env_remove("LD_PRELOAD")  // SSH doesn't like tcmalloc.
            .arg("-F").arg("None") // Don't read ~/.ssh/config.
            .arg("-p")  // Preserve access bits.
            .arg(tmp.path())
            .arg(format!("{host}:{webdir}/{relative}/{filename}"));
        debug!("Running {:?}", &command);
        let status = command
            .status().await
            .context("failed to run scp")?;
        if !status.success() {
            bail!("scp failed: {}", status);
        }
        
        urls.push(format!("https://{host}/{relative}/{filename}"));
    }

    Ok(urls)
}

/// Breaks a string into two halves, with the first half being at most `length_limit` bytes long.
fn break_line(text: &str, length_limit: usize) -> (&str, &str) {
    // unicode_word_indices gives us the start of the words, but we want the ends.
    // So we skip the first one, and then add the end of the string.
    let mut boundaries = text.unicode_word_indices().map(|(i, _)| i).skip(1).collect::<Vec<_>>();
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
    text.lines().flat_map(
        |line| {
            let mut lines = vec![];
            let mut remaining = line;
            while !remaining.is_empty() {
                let (first, rest) = break_line(remaining, length_limit);
                lines.push(first);
                remaining = rest;
            }
            lines
        }
    ).collect()
}

pub fn hash(text: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    let hash = hasher.finalize();
    hash.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    
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
        assert_eq!(hash("hello"), "ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f");
    }
}