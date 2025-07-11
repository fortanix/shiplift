//! Create and manage images.
//!
//! API Reference: <https://docs.docker.com/engine/api/v1.41/#tag/Image>

use std::{collections::HashMap, io::Read, iter};

use futures_util::{stream::Stream, TryFutureExt, TryStreamExt};
use hyper::Body;
use serde::{Deserialize, Serialize};
use url::form_urlencoded;

use crate::{docker::Docker, errors::Result, tarball, transport::tar};

#[cfg(feature = "chrono")]
use crate::datetime::datetime_from_unix_timestamp;
use crate::Error;
#[cfg(feature = "chrono")]
use chrono::{DateTime, Utc};
use bytes::Bytes;

/// Interface for accessing and manipulating a named docker image
///
/// [Api Reference](https://docs.docker.com/engine/api/v1.41/#tag/Image)
pub struct Image<'docker> {
    docker: &'docker Docker,
    name: String,
}

impl<'docker> Image<'docker> {
    /// Exports an interface for operations that may be performed against a named image
    pub fn new<S>(
        docker: &'docker Docker,
        name: S,
    ) -> Self
    where
        S: Into<String>,
    {
        Image {
            docker,
            name: name.into(),
        }
    }

    /// Inspects a named image's details
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageInspect)
    pub async fn inspect(&self) -> Result<ImageDetails> {
        self.docker
            .get_json(&format!("/images/{}/json", self.name)[..])
            .await
    }

    /// Lists the history of the images set of changes
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageHistory)
    pub async fn history(&self) -> Result<Vec<History>> {
        self.docker
            .get_json(&format!("/images/{}/history", self.name)[..])
            .await
    }

    /// Deletes an image
    /// # Arguments
    /// delete_options - delete operation options as described in API reference
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImagePrune)
    pub async fn delete_with_options(&self, delete_options: &DeleteOptions) -> Result<Vec<Status>> {
        let mut path = vec![format!("/images/{}", self.name)];

        if let Some(query) = delete_options.serialize() {
            path.push(query)
        }

        self.docker
            .delete_json::<Vec<Status>>(&path.join("?"))
            .await
    }

    /// Deletes an image
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImagePrune)
    pub async fn delete(&self) -> Result<Vec<Status>> {
        self.docker
            .delete_json::<Vec<Status>>(&format!("/images/{}", self.name)[..])
            .await
    }

    /// Export this image to a tarball
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageGet)
    pub fn export(&self) -> impl Stream<Item = Result<Vec<u8>>> + Unpin + 'docker {
        Box::pin(
            self.docker
                .stream_get(format!("/images/{}/get", self.name))
                .map_ok(|c| c.to_vec()),
        )
    }

    /// Adds a tag to an image
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageTag)
    pub async fn tag(
        &self,
        opts: &TagOptions,
    ) -> Result<()> {
        let mut path = vec![format!("/images/{}/tag", self.name)];
        if let Some(query) = opts.serialize() {
            path.push(query)
        }
        let _ = self.docker.post(&path.join("?"), None).await?;
        Ok(())
    }
}

/// Interface for docker images
pub struct Images<'docker> {
    docker: &'docker Docker,
}

impl<'docker> Images<'docker> {
    /// Exports an interface for interacting with docker images
    pub fn new(docker: &'docker Docker) -> Self {
        Images { docker }
    }

    /// Builds a new image by reading a Dockerfile in a target directory
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageBuild)
    pub fn build(
        &self,
        opts: &BuildOptions,
    ) -> impl Stream<Item = Result<ImageBuildChunk>> + Unpin + 'docker {
        let mut endpoint = vec!["/build".to_owned()];
        if let Some(query) = opts.serialize() {
            endpoint.push(query)
        }

        // To not tie the lifetime of `opts` to the 'stream, we do the tarring work outside of the
        // stream. But for backwards compatability, we have to return the error inside of the
        // stream.
        let mut bytes = Vec::default();
        let tar_result = tarball::dir(&mut bytes, opts.path.as_str(), opts.skip_gzip);

        // We must take ownership of the Docker reference. If we don't then the lifetime of 'stream
        // is incorrectly tied to `self`.
        let docker = self.docker;
        Box::pin(
            async move {
                // Bubble up error inside the stream for backwards compatability
                tar_result?;

                let value_stream = docker.stream_post_into(
                    endpoint.join("?"),
                    Some((Body::from(bytes), tar())),
                    None::<iter::Empty<_>>,
                );

                Ok(value_stream)
            }
            .try_flatten_stream(),
        )
    }

    /// Builds a new image by reading a build context
    /// # Arguments
    /// opts - options for [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageBuild)
    /// build_context - an iterator over bytes of build context archive. Format of the build context
    /// should be equivalent to that of the original `ImageBuild` function in [Api Reference].
    pub fn build_from_raw_parts<T>(
        &self,
        opts: &BuildParams,
        build_context: impl Iterator<Item = Result<T>> + Send + 'static,
    )-> impl Stream<Item = Result<ImageBuildChunk>> + Unpin + 'docker where T : Into<Bytes> + 'static {
        let mut endpoint = vec!["/build".to_owned()];
        if let Some(query) = opts.serialize() {
            endpoint.push(query)
        }

        let request_stream = futures_util::stream::iter(build_context);
        let docker = self.docker;

        Box::pin(
            async move {
                let value_stream = docker.stream_post_into(
                    endpoint.join("?"),
                    Some((Body::wrap_stream(request_stream), tar())),
                    None::<iter::Empty<_>>,
                );

                Ok(value_stream)
            }.try_flatten_stream(),
        )
    }

    /// Lists the docker images on the current docker host
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageList)
    pub async fn list(
        &self,
        opts: &ImageListOptions,
    ) -> Result<Vec<ImageInfo>> {
        let mut path = vec!["/images/json".to_owned()];
        if let Some(query) = opts.serialize() {
            path.push(query);
        }
        self.docker
            .get_json::<Vec<ImageInfo>>(&path.join("?"))
            .await
    }

    /// Returns a reference to a set of operations available for a named image
    pub fn get<S>(
        &self,
        name: S,
    ) -> Image<'docker>
    where
        S: Into<String>,
    {
        Image::new(self.docker, name)
    }

    /// Search for docker images by term
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageSearch)
    pub async fn search(
        &self,
        term: &str,
    ) -> Result<Vec<SearchResult>> {
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("term", term)
            .finish();
        self.docker
            .get_json::<Vec<SearchResult>>(&format!("/images/search?{}", query)[..])
            .await
    }

    /// Pull and create a new docker images from an existing image
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImagePull)
    pub fn pull(
        &self,
        opts: &PullOptions,
    ) -> impl Stream<Item = Result<ImageBuildChunk>> + Unpin + 'docker {
        let mut path = vec!["/images/create".to_owned()];
        if let Some(query) = opts.serialize() {
            path.push(query);
        }
        let headers = opts
            .auth_header()
            .map(|a| iter::once(("X-Registry-Auth", a)));

        Box::pin(self.docker.stream_post_into(path.join("?"), None, headers))
    }

    pub async fn push(&self, image : &str, push_options : &PushOptions) -> Result<()> {
        let mut path = vec![format!("/images/{}/push", image)];
        if let Some(query) = push_options.serialize() {
            path.push(query)
        }

        let headers = push_options
            .auth_header()
            .map(|a| iter::once(("X-Registry-Auth", a)));

        let res = self.docker.post_with_headers(&path.join("?"), None, headers).await?;
        let lines = res.split("\r\n");
        for line in lines {
            if line.contains("errorDetail") {
                return Err(Error::InvalidResponse(line.to_string()))
            }
        }
        Ok(())
    }
    
    /// exports a collection of named images,
    /// either by name, name:tag, or image id, into a tarball
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageGetAll)
    pub fn export(
        &self,
        names: Vec<&str>,
    ) -> impl Stream<Item = Result<Vec<u8>>> + 'docker {
        let params = names.iter().map(|n| ("names", *n));
        let query = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params)
            .finish();
        self.docker
            .stream_get(format!("/images/get?{}", query))
            .map_ok(|c| c.to_vec())
    }

    /// imports an image or set of images from a given tarball source
    /// source can be uncompressed on compressed via gzip, bzip2 or xz
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageLoad)
    pub fn import<R>(
        self,
        mut tarball: R,
    ) -> impl Stream<Item = Result<ImageBuildChunk>> + Unpin + 'docker
    where
        R: Read + Send + 'docker,
    {
        Box::pin(
            async move {
                let mut bytes = Vec::default();

                tarball.read_to_end(&mut bytes)?;

                let value_stream = self.docker.stream_post_into(
                    "/images/load",
                    Some((Body::from(bytes), tar())),
                    None::<iter::Empty<_>>,
                );
                Ok(value_stream)
            }
            .try_flatten_stream(),
        )
    }

    /// Deletes unused images
    ///
    /// [Api Reference](https://docs.docker.com/engine/api/v1.42/#tag/Image/operation/ImagePrune)
    pub async fn prune(
        &self,
        opts: &PruneOptions,
    ) -> Result<String> {
        let mut path = vec!["/images/prune".to_string()];

        if let Ok(Some(query)) = opts.serialize() {
            path.push(query)
        }

        self.docker.post(&path.join("?"), None).await
    }
}

#[derive(Clone, Serialize, Debug)]
#[serde(untagged)]
pub enum RegistryAuth {
    Password {
        username: String,
        password: String,

        #[serde(skip_serializing_if = "Option::is_none")]
        email: Option<String>,

        #[serde(rename = "serveraddress")]
        #[serde(skip_serializing_if = "Option::is_none")]
        server_address: Option<String>,
    },
    Token {
        #[serde(rename = "identitytoken")]
        identity_token: String,
    },
}

impl RegistryAuth {
    /// return a new instance with token authentication
    pub fn token<S>(token: S) -> RegistryAuth
    where
        S: Into<String>,
    {
        RegistryAuth::Token {
            identity_token: token.into(),
        }
    }

    /// return a new instance of a builder for authentication
    pub fn builder() -> RegistryAuthBuilder {
        RegistryAuthBuilder::default()
    }

    /// serialize authentication as JSON in base64
    pub fn serialize(&self) -> String {
        serde_json::to_string(self)
            .map(|c| base64::encode_config(&c, base64::URL_SAFE))
            .unwrap()
    }
}

#[derive(Default)]
pub struct RegistryAuthBuilder {
    username: Option<String>,
    password: Option<String>,
    email: Option<String>,
    server_address: Option<String>,
}

impl RegistryAuthBuilder {
    pub fn username<I>(
        &mut self,
        username: I,
    ) -> &mut Self
    where
        I: Into<String>,
    {
        self.username = Some(username.into());
        self
    }

    pub fn password<I>(
        &mut self,
        password: I,
    ) -> &mut Self
    where
        I: Into<String>,
    {
        self.password = Some(password.into());
        self
    }

    pub fn email<I>(
        &mut self,
        email: I,
    ) -> &mut Self
    where
        I: Into<String>,
    {
        self.email = Some(email.into());
        self
    }

    pub fn server_address<I>(
        &mut self,
        server_address: I,
    ) -> &mut Self
    where
        I: Into<String>,
    {
        self.server_address = Some(server_address.into());
        self
    }

    pub fn build(&self) -> RegistryAuth {
        RegistryAuth::Password {
            username: self.username.clone().unwrap_or_else(String::new),
            password: self.password.clone().unwrap_or_else(String::new),
            email: self.email.clone(),
            server_address: self.server_address.clone(),
        }
    }
}

#[derive(Default, Debug)]
pub struct TagOptions {
    pub params: HashMap<&'static str, String>,
}

impl TagOptions {
    /// return a new instance of a builder for options
    pub fn builder() -> TagOptionsBuilder {
        TagOptionsBuilder::default()
    }

    /// serialize options as a string. returns None if no options are defined
    pub fn serialize(&self) -> Option<String> {
        if self.params.is_empty() {
            None
        } else {
            Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&self.params)
                    .finish(),
            )
        }
    }
}

#[derive(Default)]
pub struct TagOptionsBuilder {
    params: HashMap<&'static str, String>,
}

impl TagOptionsBuilder {
    pub fn repo<R>(
        &mut self,
        r: R,
    ) -> &mut Self
    where
        R: Into<String>,
    {
        self.params.insert("repo", r.into());
        self
    }

    pub fn tag<T>(
        &mut self,
        t: T,
    ) -> &mut Self
    where
        T: Into<String>,
    {
        self.params.insert("tag", t.into());
        self
    }

    pub fn build(&self) -> TagOptions {
        TagOptions {
            params: self.params.clone(),
        }
    }
}

#[derive(Default, Debug)]
pub struct PullOptions {
    auth: Option<RegistryAuth>,
    params: HashMap<&'static str, String>,
}

impl PullOptions {
    /// return a new instance of a builder for options
    pub fn builder() -> PullOptionsBuilder {
        PullOptionsBuilder::default()
    }

    /// serialize options as a string. returns None if no options are defined
    pub fn serialize(&self) -> Option<String> {
        if self.params.is_empty() {
            None
        } else {
            Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&self.params)
                    .finish(),
            )
        }
    }

    pub(crate) fn auth_header(&self) -> Option<String> {
        self.auth.clone().map(|a| a.serialize())
    }
}

pub struct PullOptionsBuilder {
    auth: Option<RegistryAuth>,
    params: HashMap<&'static str, String>,
}

impl Default for PullOptionsBuilder {
    fn default() -> Self {
        let mut params = HashMap::new();
        params.insert("tag", "latest".to_string());

        PullOptionsBuilder { auth: None, params }
    }
}

impl PullOptionsBuilder {
    ///  Name of the image to pull. The name may include a tag or digest.
    /// This parameter may only be used when pulling an image.
    /// If an untagged value is provided and no `tag` is provided, _all_
    /// tags will be pulled
    /// The pull is cancelled if the HTTP connection is closed.
    pub fn image<I>(
        &mut self,
        img: I,
    ) -> &mut Self
    where
        I: Into<String>,
    {
        self.params.insert("fromImage", img.into());
        self
    }

    pub fn src<S>(
        &mut self,
        s: S,
    ) -> &mut Self
    where
        S: Into<String>,
    {
        self.params.insert("fromSrc", s.into());
        self
    }

    /// Repository name given to an image when it is imported. The repo may include a tag.
    /// This parameter may only be used when importing an image.
    ///
    /// By default a `latest` tag is added when calling
    /// [PullOptionsBuilder::default](PullOptionsBuilder::default].
    pub fn repo<R>(
        &mut self,
        r: R,
    ) -> &mut Self
    where
        R: Into<String>,
    {
        self.params.insert("repo", r.into());
        self
    }

    /// Tag or digest. If empty when pulling an image,
    /// this causes all tags for the given image to be pulled.
    pub fn tag<T>(
        &mut self,
        t: T,
    ) -> &mut Self
    where
        T: Into<String>,
    {
        self.params.insert("tag", t.into());
        self
    }

    pub fn auth(
        &mut self,
        auth: RegistryAuth,
    ) -> &mut Self {
        self.auth = Some(auth);
        self
    }

    pub fn build(&mut self) -> PullOptions {
        PullOptions {
            auth: self.auth.take(),
            params: self.params.clone(),
        }
    }
}

#[derive(Default, Debug)]
pub struct BuildOptions {
    path: String,

    params: HashMap<&'static str, String>,
    // Custom parameter to avoid creating a tar object of the docker
    // image when you do an image build. Instead work with a normal
    // archive buffer.
    skip_gzip: bool,
}

impl BuildOptions {
    /// return a new instance of a builder for options
    /// path is expected to be a file path to a directory containing a Dockerfile
    /// describing how to build a Docker image
    pub fn builder<S>(path: S) -> BuildOptionsBuilder
    where
        S: Into<String>,
    {
        BuildOptionsBuilder::new(path)
    }

    /// serialize options as a string. returns None if no options are defined
    pub fn serialize(&self) -> Option<String> {
        if self.params.is_empty() {
            None
        } else {
            Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&self.params)
                    .finish(),
            )
        }
    }
}

#[derive(Default)]
pub struct BuildOptionsBuilder {
    path: String,

    build_params: BuildParams,

    skip_gzip: bool,
}

impl BuildOptionsBuilder {
    /// path is expected to be a file path to a directory containing a Dockerfile
    /// describing how to build a Docker image
    pub(crate) fn new<S>(path: S) -> Self
        where
            S: Into<String>,
    {
        BuildOptionsBuilder {
            path: path.into(),
            ..Default::default()
        }
    }

    pub fn set_skip_gzip(
        &mut self,
        skip_gzip: bool,
    ) -> &mut Self {
        self.skip_gzip = skip_gzip;
        self
    }

    pub fn dockerfile<P>(
        &mut self,
        path: P,
    ) -> &mut Self
        where
            P: Into<String>,
    {
        self.build_params.dockerfile(path);
        self
    }

    pub fn tag<T>(
        &mut self,
        t: T,
    ) -> &mut Self
        where
            T: Into<String>,
    {
        self.build_params.tag(t);
        self
    }

    pub fn remote<R>(
        &mut self,
        r: R,
    ) -> &mut Self
        where
            R: Into<String>,
    {
        self.build_params.remote(r);
        self
    }

    pub fn nocache(
        &mut self,
        nc: bool,
    ) -> &mut Self {
        self.build_params.nocache(nc);
        self
    }

    pub fn rm(
        &mut self,
        r: bool,
    ) -> &mut Self {
        self.build_params.rm(r);
        self
    }

    pub fn forcerm(
        &mut self,
        fr: bool,
    ) -> &mut Self {
        self.build_params.forcerm(fr);
        self
    }

    pub fn network_mode<T>(
        &mut self,
        t: T,
    ) -> &mut Self
        where
            T: Into<String>,
    {
        self.build_params.network_mode(t);
        self
    }

    pub fn memory(
        &mut self,
        memory: u64,
    ) -> &mut Self {
        self.build_params.memory(memory);
        self
    }

    pub fn cpu_shares(
        &mut self,
        cpu_shares: u32,
    ) -> &mut Self {
        self.build_params.cpu_shares(cpu_shares);
        self
    }

    pub fn build(&self) -> BuildOptions {
        BuildOptions {
            path: self.path.clone(),
            params: self.build_params.params.clone(),
            skip_gzip: self.skip_gzip
        }
    }
}

/// Describes arguments for [Api Reference](https://docs.docker.com/engine/api/v1.41/#operation/ImageBuild)
#[derive(Default)]
pub struct BuildParams {
    params: HashMap<&'static str, String>,
}

impl BuildParams {
    /// set the name of the docker file. defaults to "DockerFile"
    pub fn dockerfile<P>(
        &mut self,
        path: P,
    ) -> &mut Self
    where
        P: Into<String>,
    {
        self.params.insert("dockerfile", path.into());
        self
    }

    /// tag this image with a name after building it
    pub fn tag<T>(
        &mut self,
        t: T,
    ) -> &mut Self
    where
        T: Into<String>,
    {
        self.params.insert("t", t.into());
        self
    }

    pub fn remote<R>(
        &mut self,
        r: R,
    ) -> &mut Self
    where
        R: Into<String>,
    {
        self.params.insert("remote", r.into());
        self
    }

    /// don't use the image cache when building image
    pub fn nocache(
        &mut self,
        nc: bool,
    ) -> &mut Self {
        self.params.insert("nocache", nc.to_string());
        self
    }

    pub fn rm(
        &mut self,
        r: bool,
    ) -> &mut Self {
        self.params.insert("rm", r.to_string());
        self
    }

    pub fn forcerm(
        &mut self,
        fr: bool,
    ) -> &mut Self {
        self.params.insert("forcerm", fr.to_string());
        self
    }

    /// `bridge`, `host`, `none`, `container:<name|id>`, or a custom network name.
    pub fn network_mode<T>(
        &mut self,
        t: T,
    ) -> &mut Self
    where
        T: Into<String>,
    {
        self.params.insert("networkmode", t.into());
        self
    }

    pub fn memory(
        &mut self,
        memory: u64,
    ) -> &mut Self {
        self.params.insert("memory", memory.to_string());
        self
    }

    pub fn cpu_shares(
        &mut self,
        cpu_shares: u32,
    ) -> &mut Self {
        self.params.insert("cpushares", cpu_shares.to_string());
        self
    }

    // todo: memswap
    // todo: cpusetcpus
    // todo: cpuperiod
    // todo: cpuquota
    // todo: buildargs

    /// serialize options as a string. returns None if no options are defined
    pub fn serialize(&self) -> Option<String> {
        if self.params.is_empty() {
            None
        } else {
            Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&self.params)
                    .finish(),
            )
        }
    }
}

/// Filter options for image listings
pub enum ImageFilter {
    Dangling,
    LabelName(String),
    Label(String, String),
}

/// Options for filtering image list results
#[derive(Default, Debug)]
pub struct ImageListOptions {
    params: HashMap<&'static str, String>,
}

impl ImageListOptions {
    pub fn builder() -> ImageListOptionsBuilder {
        ImageListOptionsBuilder::default()
    }
    pub fn serialize(&self) -> Option<String> {
        if self.params.is_empty() {
            None
        } else {
            Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&self.params)
                    .finish(),
            )
        }
    }
}

/// Builder interface for `ImageListOptions`
#[derive(Default)]
pub struct ImageListOptionsBuilder {
    params: HashMap<&'static str, String>,
}

impl ImageListOptionsBuilder {
    pub fn digests(
        &mut self,
        d: bool,
    ) -> &mut Self {
        self.params.insert("digests", d.to_string());
        self
    }

    pub fn all(&mut self) -> &mut Self {
        self.params.insert("all", "true".to_owned());
        self
    }

    /*
    /// Deprecated in v1.13
    /// Removed in v1.41
    pub fn filter_name(
        &mut self,
        name: &str,
    ) -> &mut Self {
        self.params.insert("filter", name.to_owned());
        self
    }
    */

    pub fn filter(
        &mut self,
        filters: Vec<ImageFilter>,
    ) -> &mut Self {
        let mut param = HashMap::new();
        for f in filters {
            match f {
                ImageFilter::Dangling => param.insert("dangling", vec![true.to_string()]),
                ImageFilter::LabelName(n) => param.insert("label", vec![n]),
                ImageFilter::Label(n, v) => param.insert("label", vec![format!("{}={}", n, v)]),
            };
        }
        // structure is a a json encoded object mapping string keys to a list
        // of string values
        self.params
            .insert("filters", serde_json::to_string(&param).unwrap());
        self
    }

    pub fn build(&self) -> ImageListOptions {
        ImageListOptions {
            params: self.params.clone(),
        }
    }
}

#[derive(Default, Debug)]
pub struct PushOptions {
    auth: Option<RegistryAuth>,
    params: HashMap<&'static str, String>,
}

impl PushOptions {

    pub fn builder() -> PushOptionsBuilder {
        PushOptionsBuilder::default()
    }

    fn serialize(&self) -> Option<String> {
        if self.params.is_empty() {
            None
        } else {
            Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&self.params)
                    .finish(),
            )
        }
    }

    fn auth_header(&self) -> Option<String> {
        self.auth.clone().map(|a| a.serialize())
    }
}

#[derive(Default)]
pub struct PushOptionsBuilder {
    auth: Option<RegistryAuth>,
    params: HashMap<&'static str, String>,
}

impl PushOptionsBuilder {

    pub fn tag(&mut self, t: String) -> &mut Self {
        self.params.insert("tag", t);
        self
    }

    pub fn auth(&mut self, auth: RegistryAuth) -> &mut Self {
        self.auth = Some(auth);
        self
    }

    pub fn build(&mut self) -> PushOptions {
        PushOptions {
            auth: self.auth.take(),
            params: self.params.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub description: String,
    pub is_official: bool,
    pub is_automated: Option<bool>,
    pub name: String,
    pub star_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageInfo {
    #[cfg(feature = "chrono")]
    #[serde(deserialize_with = "datetime_from_unix_timestamp")]
    pub created: DateTime<Utc>,
    #[cfg(not(feature = "chrono"))]
    pub created: u64,
    pub id: String,
    pub parent_id: String,
    pub labels: Option<HashMap<String, String>>,
    pub repo_tags: Option<Vec<String>>,
    pub repo_digests: Option<Vec<String>>,
    pub virtual_size: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageDetails {
    pub architecture: String,
    pub author: String,
    pub comment: String,
    pub config: ContainerConfig,
    #[cfg(feature = "chrono")]
    pub created: DateTime<Utc>,
    #[cfg(not(feature = "chrono"))]
    pub created: String,
    pub docker_version: String,
    pub id: String,
    pub os: String,
    pub parent: String,
    pub repo_tags: Option<Vec<String>>,
    pub repo_digests: Option<Vec<String>>,
    pub size: u64,
    pub virtual_size: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerConfig {
    pub attach_stderr: Option<bool>,
    pub attach_stdin: Option<bool>,
    pub attach_stdout: Option<bool>,
    pub cmd: Option<Vec<String>>,
    pub domainname: Option<String>,
    pub entrypoint: Option<Vec<String>>,
    pub env: Option<Vec<String>>,
    pub exposed_ports: Option<HashMap<String, HashMap<String, String>>>,
    pub hostname: Option<String>,
    pub image: Option<String>,
    pub labels: Option<HashMap<String, String>>,
    pub on_build: Option<Vec<String>>,
    pub open_stdin: Option<bool>,
    pub stdin_once: Option<bool>,
    pub tty: Option<bool>,
    pub user: String,
    pub working_dir: String,
}

impl ContainerConfig {
    pub fn env(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        if let Some(ref vars) = self.env {
            for e in vars {
                let pair: Vec<&str> = e.split('=').collect();
                map.insert(pair[0].to_owned(), pair[1].to_owned());
            }
        }
        map
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct History {
    pub id: String,
    #[cfg(feature = "chrono")]
    #[serde(deserialize_with = "datetime_from_unix_timestamp")]
    pub created: DateTime<Utc>,
    #[cfg(not(feature = "chrono"))]
    pub created: u64,
    pub created_by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Status {
    Untagged(String),
    Deleted(String),
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
/// Represents a response chunk from Docker api when building, pulling or importing an image.
pub enum ImageBuildChunk {
    Update {
        stream: String,
    },
    Error {
        error: Option<String>,
        #[serde(rename = "errorDetail")]
        error_detail: ErrorDetail,
    },
    Digest {
        aux: Aux,
    },
    PullStatus {
        status: String,
        id: Option<String>,
        progress: Option<String>,
        #[serde(rename = "progressDetail")]
        progress_detail: Option<ProgressDetail>,
    },
}

impl ImageBuildChunk {
    /// Return the eventual compressed image layer size during download (if
    /// available).
    pub fn total_image_bytes(&self) -> Option<u64> {
        match self {
            ImageBuildChunk::PullStatus {
                status: _,
                id: _,
                progress: _,
                progress_detail,
            } => {
                if let Some(detail) = progress_detail {
                    return detail.total;
                }
            }
            _ => (),
        }
        None
    }

    /// Returns the image layer ID and size during download.
    ///
    /// If both the layer ID and eventual (compressed) layer size are available
    /// from the ImageBuildChunk, they will be returned as `Some((layer_id,
    /// layer_size))`. Otherwise, `None` is returned.
    pub fn image_layer_bytes(&self) -> Option<(String, u64)> {
        match self {
            ImageBuildChunk::PullStatus {
                status: _,
                id: Some(id),
                progress: _,
                progress_detail:
                    Some(ProgressDetail {
                        current: _,
                        total: Some(total),
                    }),
            } => {
                Some((id.to_string(), *total))
            }
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Aux {
    #[serde(rename = "ID")]
    id: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ErrorDetail {
    message: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProgressDetail {
    current: Option<u64>,
    total: Option<u64>,
}

/// Describes query parameters for DELETE /images/{name}
/// https://docs.docker.com/engine/api/v1.42/#tag/Image/operation/ImageDelete
#[derive(Default, Debug)]
pub struct DeleteOptions {

    params: HashMap<&'static str, String>,
}

impl DeleteOptions {
    pub fn builder() -> DeleteOptionsBuilder {
        DeleteOptionsBuilder::default()
    }

    /// serialize options as a string. returns None if no options are defined
    pub fn serialize(&self) -> Option<String> {
        if self.params.is_empty() {
            None
        } else {
            Some(
                form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&self.params)
                    .finish(),
            )
        }
    }
}

#[derive(Default)]
pub struct DeleteOptionsBuilder {
    params: HashMap<&'static str, String>,
}

impl DeleteOptionsBuilder {

    /// Remove the image even if it is being used by stopped containers or has other tags
    pub fn force(mut self) -> Self {
        self.params.insert("force", true.to_string());
        self
    }

    /// Do not delete untagged parent images
    pub fn noprune(mut self) -> Self {
        self.params.insert("noprune", true.to_string());
        self
    }

    pub fn build(&mut self) -> DeleteOptions {
        DeleteOptions {
            params: self.params.clone(),
        }
    }
}

/// Describes query parameters for POST /images/prune
/// https://docs.docker.com/engine/api/v1.42/#tag/Image/operation/ImagePrune
#[derive(Default, Debug)]
pub struct PruneOptions {

    filters: HashMap<&'static str, Vec<String>>,
}

impl PruneOptions {

    /// serialize options as a string. returns None if no options are defined
    pub fn serialize(&self) -> Result<Option<String>> {
        if self.filters.is_empty() {
            Ok(None)
        } else {
            let value = serde_json::to_string(&self.filters).map_err(Error::from)?;

            Ok(Some(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("filters", &value)
                    .finish(),
            ))
        }
    }
}

#[derive(Default)]
pub struct PruneOptionsBuilder {
    filters: HashMap<&'static str, Vec<String>>,
}

impl PruneOptionsBuilder {

    /// dangling=<boolean> When set to true (or 1), prune only unused and untagged images.
    /// When set to false (or 0), all unused images are pruned.
    pub fn dangling(mut self, dangling: bool) -> Self {
        self.filters.insert("dangling", vec![dangling.to_string()]);
        self
    }

    /// until=<string> Prune images created before this timestamp.
    /// The <timestamp> can be Unix timestamps, date formatted timestamps, or Go duration strings (e.g. 10m, 1h30m) computed relative to the daemon machine’s time.
    pub fn until(mut self, until: String) -> Self {
        self.filters.insert("until", vec![until]);
        self
    }

    /// label (label=<key>, label=<key>=<value>, label!=<key>, or label!=<key>=<value>) Prune images with (or without, in case label!=... is used) the specified labels.
    pub fn add_label(mut self, label: String) -> Self {
        if let Some(label_list) = self.filters.get_mut("label") {
            label_list.push(label)
        } else {
            self.filters.insert("label", vec![label]);
        }

        self
    }

    pub fn add_labels<T>(mut self, labels: T) -> Self where T: Into<Vec<String>> {
        for label in labels.into() {
            self = self.add_label(label)
        }

        self
    }

    pub fn build(&mut self) -> PruneOptions {
        PruneOptions {
            filters: self.filters.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test registry auth with token
    #[test]
    fn registry_auth_token() {
        let options = RegistryAuth::token("abc");
        assert_eq!(
            base64::encode(r#"{"identitytoken":"abc"}"#),
            options.serialize()
        );
    }

    /// Test registry auth with username and password
    #[test]
    fn registry_auth_password_simple() {
        let options = RegistryAuth::builder()
            .username("user_abc")
            .password("password_abc")
            .build();
        assert_eq!(
            base64::encode(r#"{"username":"user_abc","password":"password_abc"}"#),
            options.serialize()
        );
    }

    /// Test registry auth with all fields
    #[test]
    fn registry_auth_password_all() {
        let options = RegistryAuth::builder()
            .username("user_abc")
            .password("password_abc")
            .email("email_abc")
            .server_address("https://example.org")
            .build();
        assert_eq!(
            base64::encode(
                r#"{"username":"user_abc","password":"password_abc","email":"email_abc","serveraddress":"https://example.org"}"#
            ),
            options.serialize()
        );
    }
}
