import {
  Alert,
  AlertDescription,
  AlertIcon,
  AlertTitle,
  Box,
  Button,
  HStack,
  Text,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useToast } from "@/contexts/toast";
import { SharedInstanceStartupNotification } from "@/models/shared-instance";
import { SharedInstanceService } from "@/services/shared-instance";

const notificationToastId = (notification: SharedInstanceStartupNotification) =>
  `shared-instance-startup-${notification.sharedInstanceId}`;

const SharedInstanceStartupNotifier = () => {
  const toast = useToast();
  const router = useRouter();
  const { t } = useTranslation();
  const hasChecked = useRef(false);

  useEffect(() => {
    if (hasChecked.current) return;
    hasChecked.current = true;

    const showNotifications = async () => {
      const response =
        await SharedInstanceService.retrieveStartupNotifications();
      if (response.status !== "success") {
        logger.warn(
          `Failed to check shared instance updates: ${response.details}`
        );
        return;
      }

      for (const notification of response.data) {
        const id = notificationToastId(notification);
        const openSharedInstance = () => {
          toast.close(id);
          void router.push({
            pathname: "/instances/shared",
            query: { sharedInstanceId: notification.sharedInstanceId },
          });
        };
        const isBindingPrompt = notification.kind === "bind";
        const title = t(
          isBindingPrompt
            ? "SharedInstanceStartupNotifier.bind.title"
            : "SharedInstanceStartupNotifier.update.title",
          { name: notification.name }
        );

        toast({
          id,
          duration: null,
          render: () => (
            <Alert
              status="info"
              variant="left-accent"
              alignItems="flex-start"
              borderRadius="md"
              boxShadow="lg"
              cursor="pointer"
              onClick={openSharedInstance}
            >
              <AlertIcon />
              <Box flex={1}>
                <AlertTitle>{title}</AlertTitle>
                {isBindingPrompt ? (
                  <HStack mt={2} justify="space-between">
                    <Text fontSize="sm">
                      {t("SharedInstanceStartupNotifier.bind.description")}
                    </Text>
                    <Button
                      size="xs"
                      variant="outline"
                      onClick={async (event) => {
                        event.stopPropagation();
                        const ignoreResponse =
                          await SharedInstanceService.ignoreBindingPrompt(
                            notification.sharedInstanceId
                          );
                        if (ignoreResponse.status === "success") {
                          toast.close(id);
                        } else {
                          toast({
                            title: ignoreResponse.message,
                            description: ignoreResponse.details,
                            status: "error",
                          });
                        }
                      }}
                    >
                      {t("SharedInstanceStartupNotifier.bind.ignore")}
                    </Button>
                  </HStack>
                ) : (
                  <AlertDescription mt={2}>
                    {t("SharedInstanceStartupNotifier.update.description")}
                  </AlertDescription>
                )}
              </Box>
            </Alert>
          ),
        });
      }
    };

    void showNotifications();
  }, [router, t, toast]);

  return null;
};

export default SharedInstanceStartupNotifier;
