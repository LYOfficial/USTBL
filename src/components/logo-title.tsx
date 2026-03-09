import { BoxProps, HStack, Heading, Highlight, Image } from "@chakra-ui/react";
import styles from "@/styles/logo-title.module.css";

interface LogoTitleProps extends BoxProps {}

export const TitleShort: React.FC<LogoTitleProps> = (props) => {
  return (
    <HStack spacing={2.5} {...props}>
      <Image src="/images/icons/Logo_128x128.png" alt="Logo" boxSize="26px" />
      <Heading size="md" className={styles.title}>
        <Highlight
          query="L"
          styles={{ color: "blue.600", userSelect: "none" }}
        >
          USTBL
        </Highlight>
      </Heading>
    </HStack>
  );
};

export const TitleFull: React.FC<LogoTitleProps> = (props) => {
  return (
    <Heading size="md" className={styles.title} {...props}>
      <Highlight query="L" styles={{ color: "blue.600", userSelect: "none" }}>
        USTB Launcher
      </Highlight>
    </Heading>
  );
};

export const TitleFullWithLogo: React.FC<LogoTitleProps> = (props) => {
  return (
    <HStack>
      <Image src="/images/icons/Logo_128x128.png" alt="Logo" boxSize="36px" />
      <TitleFull {...props} />
    </HStack>
  );
};
