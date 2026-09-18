import { ModalProps } from "@chakra-ui/react";
import ManageSkinModal from "@/components/modals/manage-skin-modal";
import ViewSkinModal from "@/components/modals/view-skin-modal";
import { PlayerType } from "@/enums/account";
import { Player } from "@/models/account";

interface PlayerSkinModalProps extends Omit<ModalProps, "children"> {
  player: Player;
}

const PlayerSkinModal: React.FC<PlayerSkinModalProps> = ({
  player,
  ...modalProps
}) => {
  const skin = player.textures.find(
    (texture) => texture.textureType === "SKIN"
  );
  const cape = player.textures.find(
    (texture) => texture.textureType === "CAPE"
  );

  return player.playerType === PlayerType.Offline ? (
    <ManageSkinModal
      {...modalProps}
      playerId={player.id}
      skin={skin}
      cape={cape}
    />
  ) : (
    <ViewSkinModal {...modalProps} skin={skin} cape={cape} />
  );
};

export default PlayerSkinModal;
