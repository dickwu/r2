"use client";

import { useEffect, useState } from "react";
import { PlaySquareOutlined, LoadingOutlined } from "@ant-design/icons";
import { invoke } from "@tauri-apps/api/core";

interface VideoThumbnailProps {
  src: string;
  alt?: string;
}

export default function VideoThumbnail({ src, alt }: VideoThumbnailProps) {
  const [thumbnailSrc, setThumbnailSrc] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);

  useEffect(() => {
    let cancelled = false;

    const generateThumbnail = async () => {
      try {
        // Use Rust + ffmpeg to generate thumbnail (bypasses CORS, efficient)
        const encodedUrl = new URL(src).href;
        const dataUrl = await invoke<string>("get_video_thumbnail", {
          url: encodedUrl,
        });

        if (!cancelled) {
          setThumbnailSrc(dataUrl);
          setLoading(false);
        }
      } catch (e) {
        console.error("Failed to generate thumbnail:", e);
        if (!cancelled) {
          setError(true);
          setLoading(false);
        }
      }
    };

    generateThumbnail();

    return () => {
      cancelled = true;
    };
  }, [src]);

  if (error) {
    return (
      <div className="video-thumbnail-fallback">
        <PlaySquareOutlined className="video-icon" />
      </div>
    );
  }

  return (
    <div className="video-thumbnail">
      {thumbnailSrc && (
        <img
          src={thumbnailSrc}
          alt={alt}
          style={{ display: loading ? "none" : "block" }}
        />
      )}
      {loading && (
        <div className="video-thumbnail-loading">
          <LoadingOutlined />
        </div>
      )}
      <PlaySquareOutlined className="play-icon" />
    </div>
  );
}
