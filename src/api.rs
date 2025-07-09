use anyhow::{Context, Result, anyhow};
use axum::Json;
use axum::response::IntoResponse;
use http::StatusCode;

use crate::instance::SquittalInstance;
use crate::{AppError, User, docker};

///
/// list all instances currently running. requires an authed user, and removes instance name and port from response
///
pub async fn list_instances(_: User) -> Result<impl IntoResponse, AppError> {
    let instances: Vec<SquittalInstance> = docker::get_instances()
        .await
        .context("failed to get running instances")?;

    // remove name and port from the instances so others cannot find exposed instances and mess with them
    let instances = instances
        .iter()
        .map(|iter| {
            let mut i = iter.clone();
            i.name = "".to_string();
            i.port = 0;
            return i;
        })
        .collect::<Vec<SquittalInstance>>();

    return Ok(Json(instances));
}

pub async fn get_instance(user: User) -> Result<impl IntoResponse, AppError> {
    let owner_instances = docker::get_instance_by_owner(&user.id)
        .await
        .context("failed to get instances of user")?;

    if owner_instances.len() == 0 {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    return Ok(Json(&owner_instances[0]).into_response());
}

///
/// create a new instance of the squittal container, and update the tracking in the DB
///
pub async fn create_instance(user: User) -> Result<impl IntoResponse, AppError> {
    tracing::info!("creating new instance for {}/{}", &user.id, &user.username);

    // check if owner already has an instance up
    let owner_instances = docker::get_instance_by_owner(&user.id)
        .await
        .context("failed to get instances of user")?;

    if owner_instances.len() >= 1 {
        return Ok((
            StatusCode::BAD_REQUEST,
            format!("user already has instance {}", owner_instances[0].name),
        )
            .into_response());
    }

    // make sure ink is not capped on instances created
    let instances = docker::get_instances()
        .await
        .context("failed to get running instances")?;

    if instances.len() >= 5 {
        return Ok((StatusCode::BAD_REQUEST, "already running max instances").into_response());
    }

    // user has no instances, and there is room for another one, make it!
    let container = docker::create_container(&user.id).await;
    if container.is_err() {
        return Err(anyhow!("cannot create new instance: {}", container.unwrap_err()).into());
    }

    let container: (String, u16) = container.unwrap();

    let instance: SquittalInstance = SquittalInstance {
        name: container.0,
        port: container.1.into(),
        created_by: user.id,
        created_on: std::time::SystemTime::now(),
    };

    tracing::info!(
        "created instance {} for {}/{} on port {}",
        instance.name,
        &instance.created_by,
        &user.username,
        instance.port
    );

    return Ok(Json(instance).into_response());
}

pub async fn whoami(user: Option<User>) -> impl IntoResponse {
    if user.is_some() {
        return Json(user.unwrap()).into_response();
    }

    return StatusCode::NO_CONTENT.into_response();
}
