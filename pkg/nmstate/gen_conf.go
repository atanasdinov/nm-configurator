package nmstate

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/nmstate/nmstate/rust/src/go/nmstate/v2"
	"gopkg.in/yaml.v3"
)

type nmConfig struct {
	NetworkManager [][]string `yaml:"NetworkManager"`
}

func Generate(host, configFile string) error {
	data, err := os.ReadFile(configFile)
	if err != nil {
		return fmt.Errorf("reading file: %w", err)
	}

	configuration, err := nmstate.New().GenerateConfiguration(string(data))
	if err != nil {
		return fmt.Errorf("generating configuration: %w", err)
	}

	var config nmConfig

	if err = yaml.Unmarshal([]byte(configuration), &config); err != nil {
		return fmt.Errorf("parsing configuration: %w", err)
	}

	if err = os.Mkdir(host, 0600); err != nil {
		return fmt.Errorf("creating %q dir: %w", host, err)
	}

	for _, nm := range config.NetworkManager {
		if len(nm) != 2 {
			return fmt.Errorf("invalid network manager configuration")
		}

		filename := filepath.Join(host, nm[0])
		content := nm[1]

		if err = os.WriteFile(filename, []byte(content), 0600); err != nil {
			return fmt.Errorf("writing %q file: %w", filename, err)
		}
	}

	return nil
}
