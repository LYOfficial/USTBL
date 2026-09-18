import { Box } from "@chakra-ui/react";
import { useEffect, useRef, useState } from "react";
import SkinPreview from "@/components/skin-preview";
import { Player } from "@/models/account";
import { base64ImgSrc } from "@/utils/string";

const EXPANDED_CARD_HEIGHT = 108 * 2 + 14;

interface PlayerSkinCardPreviewProps {
  player: Player;
}

const PlayerSkinCardPreview: React.FC<PlayerSkinCardPreviewProps> = ({
  player,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState<number>();
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
      const { width: containerWidth } = container.getBoundingClientRect();
      if (containerWidth > 0) {
        setWidth(Math.floor(containerWidth));
      }
    };

    updateSize();
    const observer = new ResizeObserver(updateSize);
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  return (
    <Box ref={containerRef} width="100%" height="100%" overflow="hidden">
      {width && (
        <SkinPreview
          skinSrc={skin && base64ImgSrc(skin.image)}
          capeSrc={cape && base64ImgSrc(cape.image)}
          skinModel={skin?.model}
          width={width}
          height={EXPANDED_CARD_HEIGHT}
          animation="idle"
          playEntranceAnimation
          controlBarVariant="overlay"
          showControlBar
        />
      )}
    </Box>
  );
};

export default PlayerSkinCardPreview;
