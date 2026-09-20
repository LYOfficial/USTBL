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
  VStack,
} from "@chakra-ui/react";
import { useEffect, useState } from "react";
import { LuChevronDown, LuChevronUp, LuPlus } from "react-icons/lu";
import Empty from "@/components/common/empty";
import { OptionItem, OptionItemGroup } from "@/components/common/option-item";
import { WrapCard } from "@/components/common/wrap-card";
import PlayerAvatar from "@/components/player-avatar";
import PlayerMenu from "@/components/player-menu";
import PlayerSkinCardPreview from "@/components/player-skin-card-preview";
import { useLauncherConfig } from "@/contexts/config";
import { Player } from "@/models/account";
import { generatePlayerDesc } from "@/utils/account";

const CARD_MIN_WIDTH = 160;
const CARD_HEIGHT = 108;
const CARD_GAP = 14;
const CARD_TRANSITION_MS = 280;

interface PlayersViewProps extends BoxProps {
  players: Player[];
  selectedPlayer: Player | undefined;
  viewType: string;
  onSelectCallback?: () => void;
  withMenu?: boolean;
  onCreate?: () => void;
}

const PlayersView: React.FC<PlayersViewProps> = ({
  players,
  selectedPlayer,
  viewType,
  onSelectCallback = () => {},
  withMenu = true,
  onCreate,
  ...boxProps
}) => {
  const { config, update } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const [expandedPlayerId, setExpandedPlayerId] = useState<string>();
  useEffect(() => {
    if (
      expandedPlayerId &&
      !players.some((player) => player.id === expandedPlayerId)
    ) {
      setExpandedPlayerId(undefined);
    }
  }, [expandedPlayerId, players]);

  const handleToggleExpanded = (playerId: string, isExpanded: boolean) => {
    setExpandedPlayerId(isExpanded ? undefined : playerId);
  };

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
      <PlayerMenu player={player} variant="buttonGroup" showTextureManager />
    ) : (
      <></>
    ),
  }));

  const createButton = onCreate && (
    <Button
      className="create-player-card"
      aria-label="创建角色"
      onClick={onCreate}
      variant="ghost"
      border="1px dashed"
      borderColor="gray.400"
      rounded="md"
      w="100%"
      h={viewType === "list" ? "100%" : `${CARD_HEIGHT}px`}
      position={viewType === "list" ? "absolute" : undefined}
      inset={viewType === "list" ? 0 : undefined}
      minW={0}
      fontSize="24px"
    >
      <LuPlus />
    </Button>
  );
  const createCard =
    createButton &&
    (viewType === "list" ? (
      <OptionItem
        title={"\u00a0"}
        description={"\u00a0"}
        prefixElement={<Box boxSize="32px" />}
        position="relative"
      >
        {createButton}
      </OptionItem>
    ) : (
      createButton
    ));

  return (
    <Box
      minH={onCreate ? `${CARD_HEIGHT}px` : undefined}
      {...boxProps}
      sx={{
        "& .create-player-card": {
          opacity: 0,
          transition: "opacity 150ms ease",
        },
        "&:hover .create-player-card, &:focus-within .create-player-card": {
          opacity: 1,
        },
        "@media (hover: none)": { "& .create-player-card": { opacity: 1 } },
      }}
    >
      {players.length > 0 || onCreate ? (
        <RadioGroup value={selectedPlayer?.id}>
          {viewType === "list" ? (
            <OptionItemGroup
              items={createCard ? [...listItems, createCard] : listItems}
            />
          ) : (
            <Grid
              templateColumns={`repeat(auto-fill, minmax(${CARD_MIN_WIDTH}px, 1fr))`}
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
                    rowSpan={isExpanded ? 2 : 1}
                    minW={0}
                    position="relative"
                    overflow="hidden"
                  >
                    <WrapCard
                      cardContent={
                        isExpanded ? (
                          <PlayerSkinCardPreview player={player} />
                        ) : (
                          {
                            title: player.name,
                            description: generatePlayerDesc(player, false),
                            image: (
                              <PlayerAvatar
                                avatar={player.avatar}
                                boxSize="36px"
                                objectFit="cover"
                              />
                            ),
                          }
                        )
                      }
                      variant="radio"
                      radioValue={player.id}
                      isSelected={selectedPlayer?.id === player.id}
                      onSelect={() => handleUpdateSelectedPlayer(player.id)}
                      h="100%"
                      height={`${
                        isExpanded ? CARD_HEIGHT * 2 + CARD_GAP : CARD_HEIGHT
                      }px`}
                      p={isExpanded ? 0 : undefined}
                      overflow="hidden"
                      transition={`height ${CARD_TRANSITION_MS}ms cubic-bezier(0.25, 0.8, 0.25, 1), box-shadow 0.2s ease, border-color 0.2s ease`}
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
                          <Box
                            rounded="md"
                            bg={isExpanded ? "whiteAlpha.900" : undefined}
                            color={isExpanded ? "gray.800" : undefined}
                            boxShadow={isExpanded ? "sm" : undefined}
                          >
                            <PlayerMenu
                              player={player}
                              showSkinOperation={false}
                              showTextureManager
                            />
                          </Box>
                        </Box>
                        <IconButton
                          pointerEvents="auto"
                          size="xs"
                          variant="ghost"
                          bg={isExpanded ? "whiteAlpha.900" : undefined}
                          color={isExpanded ? "gray.800" : undefined}
                          boxShadow={isExpanded ? "sm" : undefined}
                          _hover={isExpanded ? { bg: "white" } : undefined}
                          aria-label={isExpanded ? "collapse" : "expand"}
                          icon={
                            isExpanded ? <LuChevronUp /> : <LuChevronDown />
                          }
                          onClick={(event) => {
                            event.stopPropagation();
                            handleToggleExpanded(player.id, isExpanded);
                          }}
                        />
                      </VStack>
                    )}
                  </GridItem>
                );
              })}
              {createCard}
            </Grid>
          )}
        </RadioGroup>
      ) : (
        <Empty withIcon={false} size="sm" />
      )}
    </Box>
  );
};

export default PlayersView;
