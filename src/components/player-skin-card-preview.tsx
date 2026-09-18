import { Box } from "@chakra-ui/react";
import { useEffect, useRef, useState } from "react";
import SkinPreview from "@/components/skin-preview";
import { Player } from "@/models/account";
import { base64ImgSrc } from "@/utils/string";

interface PlayerSkinCardPreviewProps {
  player: Player;
}

const PlayerSkinCardPreview: React.FC<PlayerSkinCardPreviewProps> = ({
  player,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 168, height: 230 });
  const skin = player.textures.find(
    (texture) => texture.textureType === "SKIN"
  );
  const cape = player.textures.find(
    (texture) => texture.textureType === "CAPE"
  );

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const updateSize = () => {
      const { width, height } = container.getBoundingClientRect();
      if (width > 0 && height > 0) {
        setSize({ width: Math.floor(width), height: Math.floor(height) });
      }
    };

    updateSize();
    const observer = new ResizeObserver(updateSize);
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  return (
    <Box ref={containerRef} width="100%" height="100%" overflow="hidden">
      <SkinPreview
        skinSrc={skin && base64ImgSrc(skin.image)}
        capeSrc={cape && base64ImgSrc(cape.image)}
        skinModel={skin?.model}
        width={size.width}
        height={size.height}
        controlBarVariant="overlay"
        showControlBar
      />
    </Box>
  );
};

export default PlayerSkinCardPreview;
