import { Center, Icon, Text, VStack } from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuX } from "react-icons/lu";

const NotFoundPage = () => {
  const router = useRouter();
  const { t } = useTranslation();
  const [seconds, setSeconds] = useState(3);

  useEffect(() => {
    const interval = setInterval(() => {
      setSeconds((prev) => {
        if (prev === 1) {
          clearInterval(interval);
          router.push("/launch");
        }
        return prev - 1;
      });
    }, 1000);

    return () => clearInterval(interval);
  }, [router]);

  return (
    <Center h="100%">
      <VStack>
        <Icon as={LuX} w={8} h={8} color="red.500" />
        <Text>{t("NotFoundPage.text", { seconds })}</Text>
      </VStack>
    </Center>
  );
};

export default NotFoundPage;
