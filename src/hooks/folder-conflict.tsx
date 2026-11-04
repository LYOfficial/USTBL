import {
  Button,
  FormControl,
  FormErrorMessage,
  FormLabel,
  Input,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  Text,
} from "@chakra-ui/react";
import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useToast } from "@/contexts/toast";
import { GameDirectory } from "@/models/config";
import { InstanceService } from "@/services/instance";
import { getGameDirName, isInstanceNameInvalid } from "@/utils/instance";

interface PendingConflict {
  directory: GameDirectory;
  folder: string;
  suggestion?: string;
  resolve: (folder: string | null) => void;
}

interface FolderConflictDialogProps {
  pending: PendingConflict | null;
  onDone: (folder: string | null) => void;
}

const FolderConflictDialog: React.FC<FolderConflictDialogProps> = ({
  pending,
  onDone,
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const toast = useToast();
  const primaryColor = config.appearance.theme.primaryColor;
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  const [error, setError] = useState<"taken" | "invalid" | null>(null);
  const [isChecking, setIsChecking] = useState(false);

  const finish = (folder: string | null) => {
    setEditing(false);
    setError(null);
    onDone(folder);
  };

  const submit = async () => {
    if (!pending) return;
    if (isInstanceNameInvalid(value) !== 0) {
      setError("invalid");
      return;
    }
    setIsChecking(true);
    try {
      const response = await InstanceService.checkInstanceFolder(
        pending.directory,
        value
      );
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      } else if (response.data.available) {
        finish(value);
      } else {
        setError("taken");
      }
    } finally {
      setIsChecking(false);
    }
  };

  return (
    <Modal
      isOpen={pending !== null}
      onClose={() => finish(null)}
      isCentered
      closeOnOverlayClick={false}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("FolderConflictDialog.title")}</ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <Text>
            {t("FolderConflictDialog.body", {
              gameDir: pending ? getGameDirName(pending.directory) : "",
              folder: pending?.folder,
            })}
          </Text>
          {editing && (
            <FormControl mt={3} isInvalid={error !== null}>
              <FormLabel fontSize="sm">
                {t("FolderConflictDialog.label")}
              </FormLabel>
              <Input
                autoFocus
                size="sm"
                value={value}
                focusBorderColor={`${primaryColor}.500`}
                onChange={(e) => {
                  setValue(e.target.value);
                  setError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && value !== "") void submit();
                }}
              />
              {error && (
                <FormErrorMessage>
                  {t(`FolderConflictDialog.error.${error}`)}
                </FormErrorMessage>
              )}
            </FormControl>
          )}
        </ModalBody>
        <ModalFooter>
          {editing ? (
            <>
              <Button variant="ghost" onClick={() => finish(null)}>
                {t("General.cancel")}
              </Button>
              <Button
                colorScheme={primaryColor}
                disabled={value === ""}
                isLoading={isChecking}
                onClick={() => void submit()}
              >
                {t("General.confirm")}
              </Button>
            </>
          ) : (
            <>
              <Button
                variant="ghost"
                onClick={() => {
                  setValue(pending?.folder ?? "");
                  setEditing(true);
                }}
              >
                {t("FolderConflictDialog.edit")}
              </Button>
              <Button
                colorScheme={primaryColor}
                onClick={() => {
                  if (pending?.suggestion) {
                    finish(pending.suggestion);
                  } else {
                    setValue(pending?.folder ?? "");
                    setEditing(true);
                  }
                }}
              >
                {t("FolderConflictDialog.auto")}
              </Button>
            </>
          )}
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

/**
 * Settle the folder name of an instance about to be created. `resolveFolder`
 * resolves to the wanted name when it is free; otherwise it asks the user and
 * resolves to the name chosen, or null when they back out.
 */
export const useFolderConflict = () => {
  const toast = useToast();
  const [pending, setPending] = useState<PendingConflict | null>(null);

  const resolveFolder = useCallback(
    async (directory: GameDirectory, folder: string) => {
      const response = await InstanceService.checkInstanceFolder(
        directory,
        folder
      );
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
        return null;
      }
      if (response.data.available) return folder;
      return new Promise<string | null>((resolve) =>
        setPending({
          directory,
          folder,
          suggestion: response.data.suggestion ?? undefined,
          resolve,
        })
      );
    },
    [toast]
  );

  const folderConflictDialog = (
    <FolderConflictDialog
      pending={pending}
      onDone={(folder) => {
        pending?.resolve(folder);
        setPending(null);
      }}
    />
  );

  return { resolveFolder, folderConflictDialog };
};

export default useFolderConflict;
