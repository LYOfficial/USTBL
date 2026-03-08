import { Center, CenterProps, Icon, Text, VStack } from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import { LuConstruction } from "react-icons/lu";

const ComingSoonSign = ({ ...props }) => {
  const { t } = useTranslation();

  return (
    <Center h="100%" w="100%" {...(props as CenterProps)}>
      <VStack>
        <Icon as={LuConstruction} w={8} h={8} color="yellow.500" />
        <Text>{t("ComingSoonSign.text")}</Text>
      </VStack>
    </Center>
  );
};

export default ComingSoonSign;
