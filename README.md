# ink

squittal instances on demand

this is a toy project meant to help learn rust. there are much better ways to do this

word list taken from https://github.com/glitchdotcom/friendly-words

code for websocket proxying taken from https://github.com/tom-lubenow/axum-reverse-proxy

oauth2 code taken from https://github.com/tokio-rs/axum/blob/main/examples/oauth/src/main.rs

## setup

1. build squittal docker image at https://github.com/Varunda/squittal.ScrimPlanetmans/tree/conquest-linux

    make sure to use the conquest-linux branch

```
docker build -t squittal -f Dockerfile .
```

2. run mssql server image

```
docker pull mcr.microsoft.com/mssql/server:2019-latest
docker run -e "ACCEPT_EULA=Y" -e "MSSQL_SA_PASSWORD=YourStrong@Passw0rd" -p 1433:1433 --name mssql -d mcr.microsoft.com/mssql/server:2019-latest
```

3. create docker network for ink to use

```
docker network create ink
docker network connect ink mssql
```

4. run ink

```
cargo build
cargo run
```