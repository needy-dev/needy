//! Proto <-> domain mappings for `ClientService`.

use needy_api_proto::v1;

use crate::control::{ClientAccess, ClientInfo};

use super::Error;

impl From<ClientInfo> for v1::Client {
  fn from(client: ClientInfo) -> Self {
    Self {
      access: v1::ClientAccess::from(client.access).into(),
      id: client.id,
      name: client.name,
      created_at_ms: client.created_ms,
      last_used_at_ms: client.last_used_ms.unwrap_or(0),
      revoked_at_ms: client.revoked_ms.unwrap_or(0),
    }
  }
}

pub fn proto_client(client: ClientInfo) -> v1::Client {
  client.into()
}

/// Client grants must be explicit; unspecified and unknown grants fail closed.
impl TryFrom<i32> for ClientAccess {
  type Error = Error;

  fn try_from(value: i32) -> Result<Self, Error> {
    match v1::ClientAccess::try_from(value) {
      Ok(v1::ClientAccess::Runs) => Ok(Self::Runs),
      Ok(v1::ClientAccess::Unspecified) => Err(Error::Unspecified("client access")),
      Ok(v1::ClientAccess::Admin) => Ok(Self::Admin),
      Err(_) => Err(Error::Unknown {
        field: "client access",
        value,
      }),
    }
  }
}

pub fn client_access(value: i32) -> Result<ClientAccess, Error> {
  value.try_into()
}

impl From<ClientAccess> for v1::ClientAccess {
  fn from(value: ClientAccess) -> Self {
    match value {
      ClientAccess::Runs => Self::Runs,
      ClientAccess::Admin => Self::Admin,
    }
  }
}

pub fn proto_client_access(value: ClientAccess) -> v1::ClientAccess {
  value.into()
}
