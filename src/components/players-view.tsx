import {
  Box,
  BoxProps,
  Button,
  Grid,
  GridItem,
  HStack,
  IconButton,
  Radio,
  RadioGroup,
  Text,
  VStack,
  useDisclosure,
} from "@chakra-ui/react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuChevronDown, LuChevronUp } from "react-icons/lu";
import { TbHanger } from "react-icons/tb";
import Empty from "@/components/common/empty";
import { OptionItemGroup } from "@/components/common/option-item";
import { WrapCard } from "@/components/common/wrap-card";
import PlayerAvatar from "@/components/player-avatar";
import PlayerMenu from "@/components/player-menu";
import PlayerSkinModal from "@/components/player-skin-modal";
import { useLauncherConfig } from "@/contexts/config";
import { PlayerType } from "@/enums/account";
import { Player } from "@/models/account";
import { generatePlayerDesc } from "@/utils/account";

const CARD_MIN_WIDTH = 167.2;
const CARD_HEIGHT = 108;
const CARD_GAP = 14;

interface PlayersViewProps extends BoxProps {
  players: Player[];
  selectedPlayer: Player | undefined;
  viewType: string;
  onSelectCallback?: () => void;
  withMenu?: boolean;
}

const PlayersView: React.FC<PlayersViewProps> = ({
  players,
  selectedPlayer,
  viewType,
  onSelectCallback = () => {},
  withMenu = true,
  ...boxProps
}) => {
  const { t } = useTranslation();
  const { config, update } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const gridRef = useRef<HTMLDivElement>(null);
  const [columnCount, setColumnCount] = useState(1);
  const [expandedPlayerId, setExpandedPlayerId] = useState<string>();
  const [skinModalPlayer, setSkinModalPlayer] = useState<Player>();
  const {
    isOpen: isSkinModalOpen,
    onOpen: onSkinModalOpen,
    onClose: onSkinModalClose,
  } = useDisclosure();

  useEffect(() => {
    const grid = gridRef.current;
    if (!grid) return;

    const updateColumnCount = () => {
      setColumnCount(
        Math.max(
          1,
          Math.floor(
            (grid.clientWidth + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)
          )
        )
      );
    };

    updateColumnCount();
    const observer = new ResizeObserver(updateColumnCount);
    observer.observe(grid);
    return () => observer.disconnect();
  }, [viewType]);

  useEffect(() => {
    if (
      expandedPlayerId &&
      !players.some((player) => player.id === expandedPlayerId)
    ) {
      setExpandedPlayerId(undefined);
    }
  }, [expandedPlayerId, players]);

  const handleUpdateSelectedPlayer = (playerId: string) => {
    update("states.shared.selectedPlayerId", playerId);
    onSelectCallback();
  };

  const listItems = players.map((player) => ({
    title: player.name,
    description: generatePlayerDesc(player, true),
    prefixElement: (
      <HStack spacing={2.5}>
        <Radio
          value={player.id}
          onClick={() => handleUpdateSelectedPlayer(player.id)}
          colorScheme={primaryColor}
        />
        <PlayerAvatar avatar={player.avatar} boxSize="32px" objectFit="cover" />
      </HStack>
    ),
    ...(withMenu
      ? {}
      : {
          isFullClickZone: true,
          onClick: () => handleUpdateSelectedPlayer(player.id),
        }),
    children: withMenu ? (
      <PlayerMenu player={player} variant="buttonGroup" />
    ) : (
      <></>
    ),
  }));

  const handleOpenSkinModal = (player: Player) => {
    setSkinModalPlayer(player);
    onSkinModalOpen();
  };

  return (
    <Box {...boxProps}>
      {players.length > 0 ? (
        <RadioGroup value={selectedPlayer?.id}>
          {viewType === "list" ? (
            <OptionItemGroup items={listItems} />
          ) : (
            <Grid
              ref={gridRef}
              templateColumns={`repeat(${columnCount}, minmax(0, 1fr))`}
              autoRows={`${CARD_HEIGHT}px`}
              autoFlow="dense"
              gap={`${CARD_GAP}px`}
              mb={0.5}
            >
              {players.map((player) => {
                const isExpanded = expandedPlayerId === player.id;

                return (
                  <GridItem
                    key={player.id}
                    colSpan={isExpanded ? Math.min(2, columnCount) : 1}
                    rowSpan={isExpanded ? 2 : 1}
                    minW={0}
                    position="relative"
                  >
                    <WrapCard
                      cardContent={
                        <VStack spacing={0} h="100%">
                          <PlayerAvatar
                            avatar={player.avatar}
                            boxSize="36px"
                            objectFit="cover"
                          />
                          <Text
                            fontSize="xs-sm"
                            className="ellipsis-text"
                            fontWeight={
                              selectedPlayer?.id === player.id
                                ? "bold"
                                : "normal"
                            }
                            mt={2}
                            overflow="hidden"
                          >
                            {player.name}
                          </Text>
                          <Text
                            fontSize="xs"
                            className="secondary-text ellipsis-text"
                          >
                            {generatePlayerDesc(player, false)}
                          </Text>
                          {isExpanded && withMenu && (
                            <Button
                              size="xs"
                              leftIcon={<TbHanger />}
                              colorScheme={primaryColor}
                              variant="subtle"
                              mt="auto"
                              mb={2}
                              onClick={(event) => {
                                event.stopPropagation();
                                handleOpenSkinModal(player);
                              }}
                            >
                              {t(
                                `PlayerMenu.label.${
                                  player.playerType === PlayerType.Offline
                                    ? "manageSkin"
                                    : "viewSkin"
                                }`
                              )}
                            </Button>
                          )}
                        </VStack>
                      }
                      variant="radio"
                      radioValue={player.id}
                      isSelected={selectedPlayer?.id === player.id}
                      onSelect={() => handleUpdateSelectedPlayer(player.id)}
                      h="100%"
                      overflow="hidden"
                      transition="box-shadow 0.2s ease, border-color 0.2s ease"
                    />
                    {withMenu && (
                      <VStack
                        position="absolute"
                        top={0.5}
                        right={1}
                        bottom={0.5}
                        justify="space-between"
                        spacing={0}
                        pointerEvents="none"
                      >
                        <Box pointerEvents="auto">
                          <PlayerMenu
                            player={player}
                            showSkinOperation={false}
                          />
                        </Box>
                        <IconButton
                          pointerEvents="auto"
                          size="xs"
                          variant="ghost"
                          aria-label={isExpanded ? "collapse" : "expand"}
                          icon={
                            isExpanded ? <LuChevronUp /> : <LuChevronDown />
                          }
                          onClick={(event) => {
                            event.stopPropagation();
                            setExpandedPlayerId(
                              isExpanded ? undefined : player.id
                            );
                          }}
                        />
                      </VStack>
                    )}
                  </GridItem>
                );
              })}
            </Grid>
          )}
        </RadioGroup>
      ) : (
        <Empty withIcon={false} size="sm" />
      )}
      {skinModalPlayer && (
        <PlayerSkinModal
          player={skinModalPlayer}
          isOpen={isSkinModalOpen}
          onClose={onSkinModalClose}
        />
      )}
    </Box>
  );
};

export default PlayersView;
