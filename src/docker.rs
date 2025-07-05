use std::{collections::HashMap, error::Error};

use bollard::{
    query_parameters::{
        CreateContainerOptionsBuilder, InspectContainerOptions, ListContainersOptions,
        ListContainersOptionsBuilder, ListImagesOptions, ListImagesOptionsBuilder,
        RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
    },
    secret::{ContainerCreateBody, ContainerInspectResponse, HostConfig, PortBinding},
};
use rand::Rng;

use crate::instance::SquittalInstance;

pub async fn get_instances() -> Result<Vec<SquittalInstance>, bollard::errors::Error> {
    let docker = bollard::Docker::connect_with_socket_defaults()?;

    let container_filter: ListContainersOptions = ListContainersOptionsBuilder::new()
        .filters(&HashMap::from([("ancestor", vec!["squittal"])]))
        .build();

    let result = docker.list_containers(Some(container_filter)).await?;
    //println!("{:?}", result);

    let mut results: Vec<SquittalInstance> = vec![];
    for ele in result {
        results.push(SquittalInstance::from(ele));
    }

    return Ok(results);
}

///
/// get the instances created by a specific user
///
pub async fn get_instance_by_owner(
    owner: &str,
) -> Result<Vec<SquittalInstance>, bollard::errors::Error> {
    let docker = bollard::Docker::connect_with_socket_defaults()?;

    let container_filter: ListContainersOptions = ListContainersOptionsBuilder::new()
        .filters(&HashMap::from([
            ("ancestor", vec!["squittal"]),
            ("label", vec![&format!("created_by={}", owner.to_string())]),
        ]))
        .build();

    let result = docker.list_containers(Some(container_filter)).await?;
    //println!("{:?}", result);

    let mut results: Vec<SquittalInstance> = vec![];
    for ele in result {
        results.push(SquittalInstance::from(ele));
    }

    return Ok(results);
}

pub async fn get_instance_by_name(
    name: &str,
) -> Result<Vec<SquittalInstance>, bollard::errors::Error> {
    let docker = bollard::Docker::connect_with_socket_defaults()?;

    let container_filter: ListContainersOptions = ListContainersOptionsBuilder::new()
        .filters(&HashMap::from([
            ("ancestor", vec!["squittal"]),
            ("name", vec![&format!("squittal-{name}").to_string()]),
        ]))
        .build();

    let result = docker.list_containers(Some(container_filter)).await?;
    //println!("{:?}", result);

    let mut results: Vec<SquittalInstance> = vec![];
    for ele in result {
        results.push(SquittalInstance::from(ele));
    }

    return Ok(results);
}

///
/// create a new container with a discord ID set as the owner (which is stored in a label under "created_by")
///
pub async fn create_container(owner: &str) -> Result<(String, u16), Box<dyn Error>> {
    let docker = bollard::Docker::connect_with_socket_defaults()?;

    let squittal_image_filter: ListImagesOptions = ListImagesOptionsBuilder::new()
        .filters(&HashMap::from([("reference", vec!["squittal"])]))
        .build();
    match docker.list_images(Some(squittal_image_filter)).await {
        Ok(image) => {
            if image.len() != 1 {
                panic!("missing 'squittal' image! is it built?");
            }
            println!("found squittal image: {:?}", image[0]);
        }
        Err(e) => {
            return Err(Box::new(e));
        }
    }

    let instance_name = generate_container_name();
    let container_name: String = format!("squittal-{}", instance_name).to_string();
    println!("container name: {container_name}");

    let builder: CreateContainerOptionsBuilder =
        CreateContainerOptionsBuilder::new().name(&container_name);

    let config = ContainerCreateBody {
        image: Some("squittal".to_string()),
        host_config: Some(HostConfig {
            port_bindings: Some(HashMap::from([(
                "8080".to_string(),
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: None, // let Docker pick the port to use
                }]),
            )])),
            ..Default::default()
        }),
        exposed_ports: Some(HashMap::from([("8080".to_string(), HashMap::from([]))])),
        labels: Some(HashMap::from([(
            "created_by".to_string(),
            owner.to_string(),
        )])),
        ..Default::default()
    };

    let container = docker
        .create_container(Some(builder.build()), config)
        .await?;
    println!("{:?}", container);

    docker
        .start_container(&container_name, None::<StartContainerOptions>)
        .await?;

    let inspect = docker
        .inspect_container(&container_name, None::<InspectContainerOptions>)
        .await?;

    //println!("{:?}", &inspect);

    let Some(port) = get_container_port(inspect) else {
        eprintln!(
            "failed to get port of container, killing container [name={}]",
            &container_name
        );

        let _ = remove_container(&container_name).await;
        return Err("failed to get port of new instance".into());
    };

    return Ok((instance_name, port));
}

///
/// remove a container, stopping the docker container and removing it
///
pub async fn remove_container(name: &str) -> Result<(), bollard::errors::Error> {
    let docker = bollard::Docker::connect_with_socket_defaults()?;

    println!("stopping container {}", name);
    docker
        .stop_container(name, None::<StopContainerOptions>)
        .await?;

    println!("removing container {}", name);
    docker
        .remove_container(name, None::<RemoveContainerOptions>)
        .await?;

    return Ok(());
}

/**
 * generate a random container name based on the word lists
 */
fn generate_container_name() -> String {
    let first_words: Vec<String> = std::fs::read_to_string("first_word_list.txt")
        .unwrap()
        .lines()
        .map(String::from)
        .collect();

    let second_words: Vec<String> = std::fs::read_to_string("second_word_list.txt")
        .unwrap()
        .lines()
        .map(String::from)
        .collect();

    let first_word = &first_words[rand::rng().random_range(0..first_words.len())];
    let second_word = &second_words[rand::rng().random_range(0..second_words.len())];

    return format!("{first_word}-{second_word}");
}

///
/// get the host port a container is listening to 8080/tcp, which is the port used by the squittal image
///
fn get_container_port(inspect: ContainerInspectResponse) -> Option<u16> {
    let port = inspect
        .network_settings?
        .ports?
        .get("8080/tcp")?
        .as_ref()?
        .first()?
        .clone()
        .host_port?;

    match port.parse() {
        Ok(n) => Some(n),
        Err(_) => None,
    }
}
