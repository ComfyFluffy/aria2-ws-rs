use std::time::Duration;

use serde_json::{json, to_value, Map, Value};

use crate::{response, utils::PushExt, Client, Error, TaskHooks, TaskOptions};

type Result<T> = std::result::Result<T, Error>;

impl Client {
    pub async fn get_version(&self) -> Result<response::Version> {
        self.call_and_subscribe("getVersion", vec![], None).await
    }

    pub async fn add_uri(
        &self,
        uris: Vec<String>,
        options: Option<TaskOptions>,
        position: Option<u32>,
        hooks: Option<TaskHooks>,
    ) -> Result<String> {
        let mut params = vec![to_value(uris)?];
        params.push_else(options, json!({}))?;
        params.push_some(position)?;

        let gid: String = self.call_and_subscribe("addUri", params, None).await?;
        self.set_hooks(&gid, hooks);
        Ok(gid)
    }

    pub async fn add_torrent(
        &self,
        torrent: Vec<u8>,
        uris: Option<Vec<String>>,
        options: Option<TaskOptions>,
        position: Option<u32>,
        hooks: Option<TaskHooks>,
    ) -> Result<String> {
        let mut params = vec![Value::String(base64::encode(torrent))];
        params.push_else(uris, json!([]))?;
        params.push_else(options, json!({}))?;
        params.push_some(position)?;

        let gid: String = self.call_and_subscribe("addTorrent", params, None).await?;
        self.set_hooks(&gid, hooks);
        Ok(gid)
    }

    pub async fn add_metalink(
        &self,
        metalink: Vec<u8>,
        options: Option<TaskOptions>,
        position: Option<u32>,
        hooks: Option<TaskHooks>,
    ) -> Result<String> {
        let mut params = vec![Value::String(base64::encode(metalink))];
        params.push_else(options, json!({}))?;
        params.push_some(position)?;

        let gid: String = self.call_and_subscribe("addMetalink", params, None).await?;
        self.set_hooks(&gid, hooks);
        Ok(gid)
    }

    async fn do_gid(&self, method: &str, gid: String, timeout: Option<Duration>) -> Result<()> {
        self.call_and_subscribe::<String>(method, vec![Value::String(gid)], timeout)
            .await?;
        Ok(())
    }

    pub async fn remove(&self, gid: String) -> Result<()> {
        self.do_gid("remove", gid, Some(self.extendet_timeout))
            .await
    }

    pub async fn force_remove(&self, gid: String) -> Result<()> {
        self.do_gid("forceRemove", gid, None).await
    }

    pub async fn pause(&self, gid: String) -> Result<()> {
        self.do_gid("pause", gid, Some(self.extendet_timeout)).await
    }

    pub async fn pause_all(&self) -> Result<()> {
        self.call_and_subscribe::<String>("pauseAll", vec![], Some(self.extendet_timeout))
            .await?;
        Ok(())
    }

    pub async fn force_pause(&self, gid: String) -> Result<()> {
        self.do_gid("forcePause", gid, None).await
    }

    pub async fn force_pause_all(&self, gid: String) -> Result<()> {
        self.do_gid("forcePauseAll", gid, None).await
    }

    pub async fn unpause(&self, gid: String) -> Result<()> {
        self.do_gid("unpause", gid, None).await
    }

    pub async fn unpause_all(&self, gid: String) -> Result<()> {
        self.do_gid("unpauseAll", gid, None).await
    }

    pub async fn custom_tell_status(
        &self,
        gid: String,
        keys: Option<Vec<String>>,
    ) -> Result<Map<String, Value>> {
        let mut params = vec![Value::String(gid)];
        params.push_some(keys)?;
        self.call_and_subscribe("tellStatus", params, None).await
    }

    pub async fn tell_status(&self, gid: String) -> Result<response::Status> {
        self.call_and_subscribe("tellStatus", vec![Value::String(gid)], None)
            .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{from_value, json, Map};

    use crate::method::TaskOptions;

    #[test]
    fn task_options() {
        let v = json!({"a": "1", "split": "2"});
        let mut extra_options = Map::new();
        extra_options.insert("a".to_string(), json!("1"));

        assert_eq!(
            from_value::<TaskOptions>(v).unwrap(),
            TaskOptions {
                extra_options: extra_options,
                split: Some(2),
                ..Default::default()
            }
        );

        let v = json!({"split": "2"});

        assert_eq!(
            from_value::<TaskOptions>(v).unwrap(),
            TaskOptions {
                split: Some(2),
                ..Default::default()
            }
        )
    }
}
