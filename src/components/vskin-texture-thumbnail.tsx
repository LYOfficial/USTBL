import { Box, BoxProps, Image } from "@chakra-ui/react";
import { useEffect, useRef } from "react";
import { VustbTexture } from "@/models/vustb";

interface VskinTextureThumbnailProps extends BoxProps {
  texture: VustbTexture;
}

type DrawRegion = {
  source: [number, number, number, number];
  target: [number, number, number, number];
};

const drawRegion = (
  context: CanvasRenderingContext2D,
  image: HTMLImageElement,
  scale: number,
  region: DrawRegion
) => {
  const [sourceX, sourceY, sourceWidth, sourceHeight] = region.source;
  const [targetX, targetY, targetWidth, targetHeight] = region.target;
  context.drawImage(
    image,
    sourceX * scale,
    sourceY * scale,
    sourceWidth * scale,
    sourceHeight * scale,
    targetX,
    targetY,
    targetWidth,
    targetHeight
  );
};

const drawSkinThumbnail = (
  canvas: HTMLCanvasElement,
  image: HTMLImageElement,
  model: "classic" | "slim"
) => {
  const scale = image.naturalWidth / 64;
  if (
    !Number.isInteger(scale) ||
    scale < 1 ||
    image.naturalHeight < 32 * scale
  ) {
    return;
  }

  const context = canvas.getContext("2d");
  if (!context) return;
  const modernSkin = image.naturalHeight >= 64 * scale;
  const armWidth = model === "slim" ? 3 : 4;
  const leftArmX = model === "slim" ? 1 : 0;
  canvas.width = 16;
  canvas.height = 32;
  context.clearRect(0, 0, canvas.width, canvas.height);
  context.imageSmoothingEnabled = false;

  const regions: DrawRegion[] = [
    { source: [8, 8, 8, 8], target: [4, 0, 8, 8] },
    { source: [20, 20, 8, 12], target: [4, 8, 8, 12] },
    { source: [44, 20, armWidth, 12], target: [leftArmX, 8, armWidth, 12] },
    {
      source: modernSkin ? [36, 52, armWidth, 12] : [44, 20, armWidth, 12],
      target: [12, 8, armWidth, 12],
    },
    { source: [4, 20, 4, 12], target: [4, 20, 4, 12] },
    {
      source: modernSkin ? [20, 52, 4, 12] : [4, 20, 4, 12],
      target: [8, 20, 4, 12],
    },
    { source: [40, 8, 8, 8], target: [4, 0, 8, 8] },
  ];
  if (modernSkin) {
    regions.push(
      { source: [20, 36, 8, 12], target: [4, 8, 8, 12] },
      { source: [44, 36, armWidth, 12], target: [leftArmX, 8, armWidth, 12] },
      { source: [52, 52, armWidth, 12], target: [12, 8, armWidth, 12] },
      { source: [4, 36, 4, 12], target: [4, 20, 4, 12] },
      { source: [4, 52, 4, 12], target: [8, 20, 4, 12] }
    );
  }
  regions.forEach((region) => drawRegion(context, image, scale, region));
};

const VskinTextureThumbnail: React.FC<VskinTextureThumbnailProps> = ({
  texture,
  ...boxProps
}) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    if (texture.type !== "skin" || !canvasRef.current) return;
    const image = new window.Image();
    let cancelled = false;
    image.crossOrigin = "anonymous";
    image.decoding = "async";
    image.src = texture.url;
    image.onload = () => {
      if (cancelled) return;
      const canvas = canvasRef.current;
      if (canvas) drawSkinThumbnail(canvas, image, texture.model);
    };
    return () => {
      cancelled = true;
      image.onload = null;
    };
  }, [texture]);

  return (
    <Box
      display="flex"
      alignItems="center"
      justifyContent="center"
      overflow="hidden"
      bg="blackAlpha.50"
      _dark={{ bg: "whiteAlpha.100" }}
      {...boxProps}
    >
      {texture.type === "skin" ? (
        <canvas
          ref={canvasRef}
          width={16}
          height={32}
          style={{ width: "auto", height: "78%", imageRendering: "pixelated" }}
        />
      ) : (
        <Image
          src={texture.url}
          alt={texture.name}
          maxW="72%"
          maxH="72%"
          objectFit="contain"
          sx={{ imageRendering: "pixelated" }}
        />
      )}
    </Box>
  );
};

export default VskinTextureThumbnail;
