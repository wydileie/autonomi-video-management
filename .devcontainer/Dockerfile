FROM mcr.microsoft.com/devcontainers/base:dev-ubuntu-24.04
RUN apt-get update && apt-get upgrade -y \
    && apt-get -y install git curl wget unzip jq build-essential ca-certificates python3-pip python3-setuptools python3-venv \
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/* \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
    && curl -fsSL https://deb.nodesource.com/setup_18.x | bash - \
    && apt-get install -y nodejs \
    && npm install --global create-react-app

ENV PATH="/root/.cargo/bin:${PATH}"
