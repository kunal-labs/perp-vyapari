.PHONY: build test run clean help tidy-check image

build:
	@./scripts/build.sh

test:
	@./scripts/test.sh

run:
	@./scripts/run.sh

clean:
	@./scripts/clean.sh

help:
	@./scripts/help.sh

tidy-check:
	@./scripts/check-repo-tidiness.sh

image:
	@./scripts/image.sh
